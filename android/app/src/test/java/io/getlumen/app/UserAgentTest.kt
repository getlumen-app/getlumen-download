package io.getlumen.app

import io.getlumen.app.vpn.LumenVpnService
import org.junit.Assert.assertEquals
import org.junit.Test

class UserAgentTest {

    @Test
    fun userAgentCarriesTheInstalledVersion() {
        assertEquals("Lumen/${BuildConfig.VERSION_NAME}-android", LumenVpnService.USER_AGENT)
    }
}
