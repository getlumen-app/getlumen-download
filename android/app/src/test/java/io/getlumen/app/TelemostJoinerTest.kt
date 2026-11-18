package io.getlumen.app

import io.getlumen.app.vpn.TelemostJoiner
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TelemostJoinerTest {

    private val payload = JSONObject(
        TelemostJoiner.buildJoinJson("https://telemost.yandex.ru/j/123", "lumen-1"),
    )

    @Test
    fun joinRequestsReliableVideoTransport() {
        assertTrue("join payload must request KCP reliable mode", payload.optBoolean("reliable", false))
    }

    @Test
    fun joinKeepsLinkNameAndPacing() {
        assertEquals("https://telemost.yandex.ru/j/123", payload.getString("joinLink"))
        assertEquals("lumen-1", payload.getString("displayName"))
        assertEquals(60, payload.getInt("vp8Fps"))
        assertEquals(120, payload.getInt("vp8Batch"))
    }
}
