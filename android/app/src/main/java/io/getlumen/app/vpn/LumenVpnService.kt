package io.getlumen.app.vpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat
import androidbind.Androidbind
import io.getlumen.app.LumenApp
import io.getlumen.app.MainActivity
import io.getlumen.app.R
import io.getlumen.app.util.AppLog
import io.getlumen.app.util.Prefs
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.SecureRandom
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

enum class VpnStatus { IDLE, CONNECTING, CONNECTED, DEGRADED, ERROR, DISCONNECTED }

object TunnelServiceState {
    @Volatile var status: VpnStatus = VpnStatus.IDLE
    @Volatile var statusCallback: ((VpnStatus) -> Unit)? = null

    fun update(s: VpnStatus) {
        status = s
        Prefs.lastStatus = s.name.lowercase()
        statusCallback?.invoke(s)
    }
}

/**
 * Foreground VpnService that owns the whole tunnel lifecycle:
 *
 *   key -> fetch sing-box config -> verify + inject Telemost fallback ->
 *   exec libsingbox.so (mixed :10808) -> VpnService TUN -> tun2socks ->
 *   health monitor -> exec librelay.so telemost-headless-joiner on demand.
 *
 * The app package is excluded from the VPN, so the sing-box and joiner
 * processes (same UID) talk to the network directly — no loop.
 */
class LumenVpnService : VpnService() {

    companion object {
        const val TAG = "LumenVPN"
        const val CHANNEL_ID = "lumen_vpn"
        const val NOTIFICATION_ID = 1
        const val ACTION_STOP = "io.getlumen.app.STOP_VPN"

        private const val CONFIG_BASE = "https://config.getlumen.download"
        private const val USER_AGENT = "Lumen/1.0.0-android"
        private const val MAX_ROOM_ATTEMPTS = 8

        fun requestStop(context: Context) {
            val intent = Intent(context, LumenVpnService::class.java).apply { action = ACTION_STOP }
            runCatching { context.startService(intent) }
        }
    }

    @Volatile private var running = false
    @Volatile private var stopping = false
    private var vpnFd: ParcelFileDescriptor? = null
    private var workerThread: Thread? = null
    private var tunThread: Thread? = null

    private var singBox: SingBoxRunner? = null
    private var joiner: TelemostJoiner? = null
    private var healthMonitor: HealthMonitor? = null
    private var joinerCreds: Pair<String, String>? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopTunnel()
            return START_NOT_STICKY
        }
        startTunnel()
        return START_STICKY
    }

    override fun onDestroy() {
        if (running && !stopping) stopTunnel()
        super.onDestroy()
    }

    private fun startTunnel() {
        if (running || workerThread?.isAlive == true) return
        startForegroundNotification(getString(R.string.status_connecting))
        TunnelServiceState.update(VpnStatus.CONNECTING)

        workerThread = Thread({
            try {
                connectSequence()
            } catch (e: Exception) {
                AppLog.add("Connection failed: ${e.message}")
                TunnelServiceState.update(VpnStatus.ERROR)
                stopSelf()
            }
        }, "lumen-connect").also { it.start() }
    }

    private fun connectSequence() {
        val key = Prefs.proteusKey
        if (!Regex("[A-Za-z0-9_-]{8,64}").matches(key)) {
            throw IllegalStateException("Proteus key is missing or malformed")
        }

        // 1. Config (full sing-box JSON) + Telemost manifest — best-effort,
        //    the whitelist bucket/worker is reachable before the tunnel.
        AppLog.add("Fetching Proteus config…")
        val rawConfig = JSONObject(httpGet("$CONFIG_BASE/proteus-sub?sub=$key&format=json-text"))

        val embedded = rawConfig.optJSONObject("telemost_manifest")
        var manifestOk = false
        if (embedded != null) {
            val err = TelemostManifest.verify(embedded)
            manifestOk = err == null
            if (!manifestOk) AppLog.add("Embedded Telemost manifest rejected: $err")
        }
        if (!manifestOk) {
            manifestOk = TelemostManifest.fetchAndCache(filesDir, USER_AGENT) { AppLog.add(it) }
        }
        val manifest = TelemostManifest.loadCachedVerified(filesDir)
        val fallbackReady = manifest != null
        AppLog.add(if (fallbackReady) "Whitelist fallback armed (Telemost)" else "Whitelist fallback unavailable")

        // 2. Per-session SOCKS credentials for the local joiner outbound.
        val rand = SecureRandom()
        val creds = randomString(rand, 16) to randomString(rand, 24)
        joinerCreds = creds

        // 3. Transform + write config.
        val result = ConfigTransform.apply(
            rawConfig,
            File(filesDir, "cache.db").absolutePath,
            creds.first, creds.second, fallbackReady,
        )
        result.notes.forEach { AppLog.add("config: $it") }
        val configPath = File(filesDir, "sing-box.json")
        configPath.writeText(result.config.toString())

        // 4. sing-box.
        val sb = SingBoxRunner(applicationInfo.nativeLibraryDir, filesDir) { AppLog.add(it) }
        singBox = sb
        sb.start(configPath.absolutePath)
        if (!sb.awaitPort(ConfigTransform.MIXED_PORT, 20_000)) {
            throw IllegalStateException("sing-box did not open port ${ConfigTransform.MIXED_PORT}")
        }
        AppLog.add("sing-box up on :${ConfigTransform.MIXED_PORT}")

        // 5. TUN.
        val builder = Builder()
            .setSession("Lumen")
            .addAddress("10.0.0.2", 32)
            .addRoute("0.0.0.0", 0)
            .setMtu(1500)
        runCatching {
            builder.addAddress("fd00::2", 128)
            builder.addRoute("::", 0)
        }
        builder.addDnsServer("8.8.8.8")
        builder.addDnsServer("8.8.4.4")
        runCatching { builder.addDisallowedApplication(packageName) }

        vpnFd = builder.establish() ?: throw IllegalStateException("VpnService.establish() returned null")
        val fd = vpnFd!!.detachFd()

        tunThread = Thread({
            try {
                Androidbind.startTun2Socks(fd.toLong(), 1500L, ConfigTransform.MIXED_PORT.toLong(), "", "")
            } catch (e: Exception) {
                Log.e(TAG, "tun2socks: ${e.message}")
                AppLog.add("tun2socks error: ${e.message}")
            }
        }, "tun2socks").also { it.start() }

        running = true
        TunnelServiceState.update(VpnStatus.CONNECTED)
        startForegroundNotification(getString(R.string.status_connected))
        AppLog.add("Tunnel established")

        // 6. Health loop → Telemost fallback on persistent foreign failure.
        healthMonitor = HealthMonitor(
            mixedPort = ConfigTransform.MIXED_PORT,
            fallbackAvailable = { fallbackReady },
            fallbackAlive = { joiner?.isRunning == true },
            activateFallback = { activateFallback(manifest) },
            onLog = { AppLog.add(it) },
            onDegraded = { degraded ->
                TunnelServiceState.update(if (degraded) VpnStatus.DEGRADED else VpnStatus.CONNECTED)
                startForegroundNotification(
                    getString(if (degraded) R.string.status_degraded else R.string.status_connected)
                )
            },
        ).also { it.start() }
    }

    /**
     * Rotate manifest rooms (seeded order) until one joiner passes a real
     * data-plane probe through its SOCKS port. Single-room port contract:
     * the joiner always serves on TELEMOST_LOCAL_PORT.
     */
    private fun activateFallback(manifest: JSONObject?): Boolean {
        val m = manifest ?: TelemostManifest.loadCachedVerified(filesDir) ?: return false
        val candidates = TelemostManifest.selectRoomLinks(m, TelemostManifest.clientSeed(filesDir))
        if (candidates.isEmpty()) return false
        val creds = joinerCreds ?: return false

        candidates.take(MAX_ROOM_ATTEMPTS).forEachIndexed { i, link ->
            val tj = TelemostJoiner(
                applicationInfo.nativeLibraryDir,
                ConfigTransform.TELEMOST_LOCAL_PORT,
                creds.first, creds.second,
                onLog = { AppLog.add(it) },
                onStatus = { },
            )
            joiner = tj
            tj.start(link, "lumen-android-$i")
            if (!tj.awaitConnected(35_000)) {
                AppLog.add("telemost room ${i + 1} did not connect, rotating")
                tj.stop()
                return@forEachIndexed
            }
            if (SocksProbe.probe(
                    ConfigTransform.TELEMOST_LOCAL_PORT,
                    user = creds.first, pass = creds.second
                )
            ) {
                return true
            }
            AppLog.add("telemost room ${i + 1} failed data-plane probe, rotating")
            tj.stop()
        }
        return false
    }

    private fun httpGet(url: String): String {
        val conn = (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 15_000
            readTimeout = 15_000
            setRequestProperty("User-Agent", USER_AGENT)
        }
        try {
            if (conn.responseCode !in 200..299) {
                throw IllegalStateException("config fetch failed: HTTP ${conn.responseCode}")
            }
            return conn.inputStream.readBytes().toString(Charsets.UTF_8)
        } finally {
            conn.disconnect()
        }
    }

    private fun randomString(rand: SecureRandom, len: Int): String {
        val chars = "abcdefghijklmnopqrstuvwxyz0123456789"
        return buildString { repeat(len) { append(chars[rand.nextInt(chars.length)]) } }
    }

    @Synchronized
    fun stopTunnel() {
        if (stopping) return
        stopping = true
        running = false
        healthMonitor?.stop()
        healthMonitor = null
        joiner?.stop()
        joiner = null
        singBox?.stop()
        singBox = null

        val done = CountDownLatch(1)
        Thread {
            runCatching { Androidbind.stopTun2Socks() }
            done.countDown()
        }.start()
        done.await(2000, TimeUnit.MILLISECONDS)

        runCatching { vpnFd?.close() }
        vpnFd = null
        workerThread = null

        runCatching { @Suppress("DEPRECATION") stopForeground(true) }
        TunnelServiceState.update(VpnStatus.DISCONNECTED)
        stopping = false
        stopSelf()
    }

    private fun startForegroundNotification(text: String) {
        val nm = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Lumen VPN", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val openIntent = PendingIntent.getActivity(
            this, 1,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val stopIntent = PendingIntent.getService(
            this, 2,
            Intent(this, LumenVpnService::class.java).apply { action = ACTION_STOP },
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setOngoing(true)
            .setContentIntent(openIntent)
            .addAction(0, getString(R.string.notification_disconnect), stopIntent)
            .build()
        startForeground(NOTIFICATION_ID, notification)
    }
}
