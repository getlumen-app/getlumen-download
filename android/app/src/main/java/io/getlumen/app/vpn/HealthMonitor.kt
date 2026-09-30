package io.getlumen.app.vpn

/**
 * Backend health loop, ported from the desktop `health_monitor.rs`: probes
 * the real data path (through the local sing-box SOCKS port) every 10s after
 * a 20s warmup; two consecutive failures with a verified manifest activate
 * the Telemost fallback; a recovered probe clears the degraded flag while the
 * joiner stays up so a re-degrade is cheap.
 */
class HealthMonitor(
    private val mixedPort: Int,
    private val fallbackAvailable: () -> Boolean,
    private val fallbackAlive: () -> Boolean,
    private val activateFallback: () -> Boolean,
    private val onLog: (String) -> Unit,
    private val onDegraded: (Boolean) -> Unit,
) {
    companion object {
        const val WARMUP_MS = 20_000L
        const val INTERVAL_MS = 10_000L
        const val FAILURES_TO_SWITCH = 2
    }

    @Volatile
    private var running = false
    private var thread: Thread? = null

    fun start() {
        stop()
        running = true
        thread = Thread({ loop() }, "health-monitor").also { it.start() }
    }

    fun stop() {
        running = false
        thread?.interrupt()
        thread = null
    }

    private fun loop() {
        Thread.sleep(WARMUP_MS)
        var failures = 0
        var fallbackStarted = false
        var degraded = false
        while (running) {
            val ok = SocksProbe.probe(mixedPort)
            failures = if (ok) 0 else failures + 1

            if (fallbackStarted && !fallbackAlive()) {
                onLog("Telemost joiner exited — will re-activate on failures")
                fallbackStarted = false
            }

            if (!fallbackStarted && failures >= FAILURES_TO_SWITCH && fallbackAvailable()) {
                onLog("$failures probe failures — activating Telemost fallback")
                fallbackStarted = activateFallback()
                if (fallbackStarted) {
                    onLog("Telemost fallback active on :${ConfigTransform.TELEMOST_LOCAL_PORT}")
                } else {
                    onLog("Telemost fallback failed to start")
                    failures = 0 // back off: retry on the next failure streak
                }
            }

            val show = fallbackStarted && failures >= FAILURES_TO_SWITCH
            if (show != degraded) {
                degraded = show
                onDegraded(show)
            }

            Thread.sleep(INTERVAL_MS)
        }
    }
}
