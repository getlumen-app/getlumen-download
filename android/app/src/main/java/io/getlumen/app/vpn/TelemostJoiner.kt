package io.getlumen.app.vpn

import org.json.JSONObject
import java.io.BufferedWriter
import java.io.File
import java.io.OutputStreamWriter
import java.net.Inet4Address
import java.net.InetAddress
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Drives the bundled relay binary in `telemost-headless-joiner` mode: it
 * prints STATUS:/RESOLVE: lines, accepts a `JOIN:<json>` command on stdin and
 * keeps a persistent local SOCKS5 listener bridged into the Telemost room.
 *
 * DNS for the joiner goes through the RESOLVE: protocol — a static Go binary
 * cannot use the Android resolver — so each lookup is answered by the Java
 * runtime here.
 */
class TelemostJoiner(
    private val nativeLibDir: String,
    private val socksPort: Int,
    private val socksUser: String,
    private val socksPass: String,
    private val onLog: (String) -> Unit,
    private val onStatus: (String) -> Unit,
) {
    private var process: Process? = null
    private var stdinWriter: BufferedWriter? = null
    private var readerThread: Thread? = null
    private val pendingCommands = mutableListOf<String>()

    @Volatile
    var tunnelConnected = false
        private set

    @Volatile
    var isRunning = false
        private set

    private var joinJson: String? = null
    @Volatile
    private var connectedLatch = CountDownLatch(1)

    fun start(joinLink: String, displayName: String) {
        stop()
        tunnelConnected = false
        connectedLatch = CountDownLatch(1)

        val binary = File(nativeLibDir, "librelay.so")
        check(binary.exists()) { "librelay.so missing" }

        joinJson = JSONObject().apply {
            put("joinLink", joinLink)
            put("displayName", displayName)
            put("vp8Fps", 60)
            put("vp8Batch", 120)
        }.toString()

        isRunning = true
        readerThread = Thread({
            try {
                runLoop(binary)
            } catch (e: Exception) {
                if (isRunning) {
                    onLog("telemost joiner error: ${e.message}")
                    onStatus("error")
                }
            } finally {
                isRunning = false
            }
        }, "telemost-joiner").also { it.start() }
    }

    private fun runLoop(binary: File) {
        val pb = ProcessBuilder(
            binary.absolutePath,
            "--mode", "telemost-headless-joiner",
            "--ws-port", "9011",
            "--socks-host", "127.0.0.1",
            "--socks-port", socksPort.toString(),
            "--socks-user", socksUser,
            "--socks-pass", socksPass,
        )
        pb.redirectErrorStream(true)
        val proc = pb.start()
        synchronized(this) {
            process = proc
            stdinWriter = BufferedWriter(OutputStreamWriter(proc.outputStream))
            pendingCommands.forEach { writeStdinLocked(it) }
            pendingCommands.clear()
        }
        onLog("telemost joiner started on :$socksPort")

        proc.inputStream.bufferedReader().forEachLine { line ->
            when {
                line.startsWith("RESOLVE:") -> {
                    val host = line.removePrefix("RESOLVE:")
                    val ip = runCatching {
                        InetAddress.getAllByName(host)
                            .firstOrNull { it is Inet4Address }
                            ?.hostAddress
                            ?: InetAddress.getByName(host).hostAddress
                    }.getOrNull() ?: ""
                    writeStdin(ip)
                }
                line.startsWith("STATUS:") -> {
                    when (val status = line.removePrefix("STATUS:")) {
                        "READY" -> joinJson?.let { writeStdin("JOIN:$it") }
                        "TUNNEL_CONNECTED" -> {
                            tunnelConnected = true
                            connectedLatch.countDown()
                            onStatus("connected")
                        }
                        "TUNNEL_LOST" -> {
                            tunnelConnected = false
                            onStatus("lost")
                        }
                        else -> if (status.startsWith("ERROR:")) {
                            onLog("telemost joiner: $status")
                            onStatus("error")
                        }
                    }
                }
                else -> onLog("joiner: $line")
            }
        }
        proc.waitFor()
    }

    /** True when the tunnel is established within the timeout. */
    fun awaitConnected(timeoutMs: Long): Boolean {
        return runCatching {
            connectedLatch.await(timeoutMs, TimeUnit.MILLISECONDS)
        }.getOrDefault(false)
    }

    @Synchronized
    private fun writeStdin(line: String) {
        val writer = stdinWriter ?: run {
            pendingCommands.add(line)
            return
        }
        writeStdinLocked(line)
    }

    private fun writeStdinLocked(line: String) {
        runCatching {
            stdinWriter?.write(line)
            stdinWriter?.newLine()
            stdinWriter?.flush()
        }
    }

    @Synchronized
    fun stop() {
        isRunning = false
        runCatching { stdinWriter?.close() }
        stdinWriter = null
        val proc = process
        process = null
        proc?.destroy()
        if (proc != null) {
            runCatching {
                if (!proc.waitFor(1200, TimeUnit.MILLISECONDS)) {
                    proc.destroyForcibly()
                    proc.waitFor(400, TimeUnit.MILLISECONDS)
                }
            }
        }
        readerThread?.interrupt()
        readerThread = null
    }
}
