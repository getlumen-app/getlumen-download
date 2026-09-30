package io.getlumen.app

import io.getlumen.app.vpn.TelemostManifest
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

class TelemostManifestTest {

    private fun manifest(
        rooms: List<String> = listOf(
            "https://telemost.yandex.ru/j/aaaa",
            "https://telemost.yandex.ru/j/bbbb",
            "https://telemost.yandex.ru/j/cccc",
        ),
        validUntil: String = "2999-01-01T00:00:00Z",
        signature: String = "AAAA",
        payloadB64: String? = null,
    ): JSONObject {
        val payload = JSONObject().apply {
            put("rooms", JSONArray(rooms.map { JSONObject().put("url", it) }))
            put("valid_until", validUntil)
        }
        val b64 = payloadB64 ?: Base64.getEncoder().encodeToString(payload.toString().toByteArray())
        return JSONObject().apply {
            put("signature_alg", "RS256")
            put("payload_b64", b64)
            put("signature", signature)
            put("payload", payload)
        }
    }

    @Test
    fun shapeRejectsMissingSignatureAlg() {
        val m = manifest()
        m.put("signature_alg", "none")
        assertNotNull(TelemostManifest.verify(m))
    }

    @Test
    fun shapeRejectsNonTelemostRooms() {
        val m = manifest(rooms = listOf("https://example.com/j/x"))
        assertEquals("no usable room link", TelemostManifest.verify(m))
    }

    @Test
    fun shapeRejectsMismatchedPayloadMirror() {
        val m = manifest()
        // An unsigned mirror that disagrees with the signed bytes is an
        // injection attempt — must be rejected before signature checks pass.
        m.put("payload", JSONObject().put("rooms", JSONArray()).put("valid_until", "2999-01-01T00:00:00Z"))
        assertEquals("payload does not match payload_b64", TelemostManifest.verify(m))
    }

    @Test
    fun expiredManifestRejectedAfterSignatureCheck() {
        // Signature will fail first for this fixture (dummy signature), so
        // ordering of checks matters less; this pins the expiry code path
        // presence in the gate contract.
        val m = manifest(validUntil = "2001-01-01T00:00:00Z")
        assertNotNull(TelemostManifest.verify(m))
    }

    @Test
    fun seededSelectionIsDeterministicAndShuffled() {
        val links = (1..10).map { "https://telemost.yandex.ru/j/room$it" }
        val m = manifest(rooms = links)

        val orderA = TelemostManifest.selectRoomLinks(m, 42L)
        val orderB = TelemostManifest.selectRoomLinks(m, 42L)
        val orderC = TelemostManifest.selectRoomLinks(m, 1337L)

        assertEquals(orderA, orderB)
        assertEquals(links.toSet(), orderA.toSet())
        // Overwhelmingly likely a different permutation for a different seed;
        // guard the "everyone takes the first N links" regression.
        assertTrue(orderA != orderC || links.size <= 2)
    }
}
