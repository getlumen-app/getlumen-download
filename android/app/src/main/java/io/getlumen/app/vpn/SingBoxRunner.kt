package io.getlumen.app.vpn

import java.io.File
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.TimeUnit

/**
 * Runs the bundled sing-box binary (`libsingbox.so`, extracted to
 * nativeLibraryDir so it carries exec permission under Android SELinux).
 * The fetched config only uses a local `mixed` inbound — no TUN, no root.
 */
class SingBoxRunner(
    private val nativeLibDir: String,
    private val filesDir: File,
    private val onLog: (String) -> Unit,
) {
    @Volatile
    var process: Process? = null
        private set

    private var readerThread: Thread? = null

    val isRunning: Boolean
        get() = process?.isAlive == true

    fun start(configPath: String) {
        val binary = File(nativeLibDir, "libsingbox.so")
        check(binary.exists()) { "libsingbox.so missing" }

        val pb = ProcessBuilder(
            binary.absolutePath,
            "run",
            "-c", configPath,
            "-D", filesDir.absolutePath,
            "--disable-color",
        )
        pb.environment()["TMPDIR"] = File(filesDir, "tmp").apply { mkdirs() }.absolutePath
        // Static Go binary: crypto/x509 scans SSL_CERT_DIR for the CA store —
        // Android keeps it at a fixed well-known path apps can read.
        pb.environment()["SSL_CERT_DIR"] = "/system/etc/security/cacerts"
        pb.redirectErrorStream(true)
        val proc = pb.start()
        process = proc
        readerThread = Thread({
            proc.inputStream.bufferedReader().forEachLine { line ->
                if (isNoisyLine(line)) return@forEachLine
                onLog("sing-box: $line")
            }
        }, "singbox-log").also { it.start() }
    }

    /**
     * Inside an app sandbox the netlink route dump is denied, so the
     * interface monitor retries once a second forever — a steady stream of
     * identical ERROR lines that is cosmetic only (dialers simply fall back
     * to the default path). Hide it from the in-app log.
     */
    private fun isNoisyLine(line: String): Boolean =
        line.contains("netlinkrib") || line.contains("network monitor unavailable")

    /** Waits until the mixed SOCKS/HTTP inbound accepts a TCP connection. */
    fun awaitPort(port: Int, timeoutMs: Long): Boolean {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (process?.isAlive == false) return false
            runCatching {
                Socket().use { it.connect(InetSocketAddress("127.0.0.1", port), 400) }
            }.onSuccess { return true }
            Thread.sleep(300)
        }
        return false
    }

    @Synchronized
    fun stop() {
        val proc = process ?: return
        process = null
        proc.destroy()
        try {
            if (!proc.waitFor(1500, TimeUnit.MILLISECONDS)) {
                proc.destroyForcibly()
                proc.waitFor(500, TimeUnit.MILLISECONDS)
            }
        } catch (_: Exception) {
        }
        readerThread?.interrupt()
        readerThread = null
    }
}
