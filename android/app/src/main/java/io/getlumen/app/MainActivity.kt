package io.getlumen.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import io.getlumen.app.util.AppLog
import io.getlumen.app.util.Prefs
import io.getlumen.app.vpn.LumenVpnService
import io.getlumen.app.vpn.TunnelServiceState
import io.getlumen.app.vpn.VpnStatus

class MainActivity : AppCompatActivity() {

    private lateinit var keyInput: EditText
    private lateinit var connectButton: Button
    private lateinit var statusText: TextView
    private lateinit var logView: TextView
    private lateinit var spinner: ProgressBar

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildContentView())
        requestNotificationPermission()
        refreshStatus(TunnelServiceState.status)
        logView.text = AppLog.snapshot().joinToString("\n")
    }

    override fun onResume() {
        super.onResume()
        TunnelServiceState.statusCallback = { runOnUiThread { refreshStatus(it) } }
        AppLog.listener = { lines -> runOnUiThread { logView.text = lines.joinToString("\n") } }
        refreshStatus(TunnelServiceState.status)
    }

    override fun onPause() {
        TunnelServiceState.statusCallback = null
        AppLog.listener = null
        super.onPause()
    }

    private fun buildContentView(): View {
        val pad = dp(20)
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad, pad, pad)
        }

        root.addView(TextView(this).apply {
            text = getString(R.string.app_name)
            textSize = 28f
            setTypeface(typeface, Typeface.BOLD)
        })
        root.addView(TextView(this).apply {
            text = getString(R.string.app_tagline)
            textSize = 13f
            setTextColor(0xFF666666.toInt())
            setPadding(0, 0, 0, dp(16))
        })

        keyInput = EditText(this).apply {
            hint = getString(R.string.key_hint)
            setText(Prefs.proteusKey)
            setSingleLine(true)
        }
        root.addView(keyInput)

        val statusRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dp(16), 0, 0)
        }
        spinner = ProgressBar(this).apply {
            layoutParams = LinearLayout.LayoutParams(dp(22), dp(22))
            visibility = View.GONE
        }
        statusRow.addView(spinner)
        statusText = TextView(this).apply {
            textSize = 16f
            setPadding(dp(10), 0, 0, 0)
        }
        statusRow.addView(statusText)
        root.addView(statusRow)

        connectButton = Button(this).apply {
            text = getString(R.string.action_connect)
            setOnClickListener { onConnectClicked() }
        }
        root.addView(connectButton, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT
        ).apply { topMargin = dp(16) })

        root.addView(TextView(this).apply {
            text = getString(R.string.log_label)
            textSize = 12f
            setTextColor(0xFF888888.toInt())
            setPadding(0, dp(20), 0, dp(6))
        })

        val logScroll = ScrollView(this)
        logView = TextView(this).apply {
            textSize = 11f
            typeface = Typeface.MONOSPACE
            setTextIsSelectable(true)
        }
        logScroll.addView(logView)
        root.addView(logScroll, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f
        ))

        return root
    }

    private fun onConnectClicked() {
        val current = TunnelServiceState.status
        if (current == VpnStatus.CONNECTED || current == VpnStatus.DEGRADED ||
            current == VpnStatus.CONNECTING
        ) {
            LumenVpnService.requestStop(this)
            refreshStatus(VpnStatus.DISCONNECTED)
            return
        }

        val key = keyInput.text.toString().trim()
        if (key.isEmpty()) {
            keyInput.error = getString(R.string.key_required)
            return
        }
        Prefs.proteusKey = key

        val intent = VpnService.prepare(this)
        if (intent != null) {
            startActivityForResult(intent, REQUEST_VPN)
        } else {
            startVpn()
        }
    }

    private fun startVpn() {
        val intent = Intent(this, LumenVpnService::class.java)
        ContextCompat.startForegroundService(this, intent)
        refreshStatus(VpnStatus.CONNECTING)
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQUEST_VPN) {
            if (resultCode == RESULT_OK) startVpn() else refreshStatus(VpnStatus.IDLE)
        }
    }

    private fun refreshStatus(status: VpnStatus) {
        val res = when (status) {
            VpnStatus.IDLE -> R.string.status_idle
            VpnStatus.CONNECTING -> R.string.status_connecting
            VpnStatus.CONNECTED -> R.string.status_connected
            VpnStatus.DEGRADED -> R.string.status_degraded
            VpnStatus.ERROR -> R.string.status_error
            VpnStatus.DISCONNECTED -> R.string.status_disconnected
        }
        statusText.text = getString(res)
        spinner.visibility = if (status == VpnStatus.CONNECTING) View.VISIBLE else View.GONE
        connectButton.text = getString(
            if (status == VpnStatus.CONNECTED || status == VpnStatus.DEGRADED ||
                status == VpnStatus.CONNECTING
            ) R.string.action_disconnect else R.string.action_connect
        )
    }

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()

    companion object {
        private const val REQUEST_VPN = 1001
        private const val REQUEST_NOTIFICATIONS = 1002
    }
}
