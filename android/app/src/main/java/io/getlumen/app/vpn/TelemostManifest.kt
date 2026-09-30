package io.getlumen.app.vpn

import io.getlumen.app.util.B64
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.KeyFactory
import java.security.SecureRandom
import java.security.Signature
import java.security.spec.X509EncodedKeySpec
import java.text.SimpleDateFormat
import java.util.Locale
import java.util.TimeZone

/**
 * Signed Telemost room manifest: fetch, RS256 verification against the
 * embedded trust root, per-install seeded room ordering, disk cache.
 *
 * Contract mirrored from the desktop client (config.rs): the manifest signs
 * `payload_b64`; the decoded bytes are the authoritative room list.
 * A Telemost conference carries exactly one active joiner, so clients spread
 * across the shared pool via a persisted random seed.
 */
object TelemostManifest {

    const val FETCH_URL = "https://config.getlumen.download/telemost-manifest.json"
    private const val LINK_PREFIX = "https://telemost.yandex.ru/j/"
    private const val CACHE_FILE = "telemost-manifest.json"
    private const val SEED_FILE = "telemost-seed"

    private const val PUBLIC_PEM = """
-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAoun0H6h6gRUeQypWtV6J
Y+9yeT9cSvrq0F1MnB+NzZtvIntq4ghrf4uuzcpgeHBaeX2hqcTK1NVwVT2bYTUu
/g9rP/WgvMjU+mrLJ+Rm0KSJ0OQGLL4HXgfp9E8qZfnpQLqcLjJrvg0C1n//apD5
kSGQ6hw2jOJs7foxP3cPvCFfeZ3DtfQ5PM8w6POQXWYT10YYSP0ow+FCZCAUBMho
SG5BJxbL9D/0kPPXH4LmvUF8cnc/ohG68xCJ3B96ynGs+l9rvXrl6VkQsSsHrEEL
+x13MyayPOeDOOhPodN6UR0YLHNn6Rkufdfj0jKPvP6jqgbCEgDrgTWE+zpp6sJP
4QIDAQAB
-----END PUBLIC KEY-----
"""

    private fun manifestFile(filesDir: File) = File(filesDir, CACHE_FILE)
    private fun seedFile(filesDir: File) = File(filesDir, SEED_FILE)

    /** Fetch, verify and cache the manifest. Returns true on a verified cache. */
    fun fetchAndCache(filesDir: File, userAgent: String, log: (String) -> Unit): Boolean {
        val body = runCatching {
            val conn = (URL(FETCH_URL).openConnection() as HttpURLConnection).apply {
                connectTimeout = 15_000
                readTimeout = 15_000
                setRequestProperty("User-Agent", userAgent)
            }
            try {
                if (conn.responseCode !in 200..299) return false
                conn.inputStream.readBytes()
            } finally {
                conn.disconnect()
            }
        }.getOrElse {
            log("Telemost manifest fetch failed: ${it.message}")
            return false
        }

        val manifest = runCatching { JSONObject(String(body)) }.getOrNull() ?: return false
        val err = verify(manifest)
        if (err != null) {
            log("Telemost manifest rejected: $err")
            return false
        }
        runCatching { manifestFile(filesDir).writeText(manifest.toString()) }
        log("Telemost manifest verified (${roomLinks(manifest).size} rooms)")
        return true
    }

    fun loadCachedVerified(filesDir: File): JSONObject? {
        val manifest = runCatching {
            JSONObject(manifestFile(filesDir).readText())
        }.getOrNull() ?: return null
        return if (verify(manifest) == null) manifest else null
    }

    /**
     * Full fallback gate: shape + RS256 over payload_b64 + non-expired
     * valid_until. Returns null when the manifest is trustworthy.
     */
    fun verify(manifest: JSONObject): String? {
        if (manifest.optString("signature_alg") != "RS256") return "unsupported signature_alg"
        val payloadB64 = manifest.optString("payload_b64").ifEmpty { return "missing payload_b64" }
        val signatureB64 = manifest.optString("signature").ifEmpty { return "missing signature" }

        val payloadBytes = runCatching { B64.decode(payloadB64) }
            .getOrElse { return "payload_b64 decode failed" }
        val signatureBytes = runCatching { B64.decode(signatureB64) }
            .getOrElse { return "signature decode failed" }

        // Signed bytes are authoritative: parse rooms from payload_b64, and
        // when an unsigned `payload` mirror is present require it to match.
        val decoded = runCatching { JSONObject(String(payloadBytes)) }
            .getOrElse { return "payload_b64 is not JSON" }
        val mirror = manifest.optJSONObject("payload")
        if (mirror != null && !jsonEquals(mirror, decoded)) return "payload does not match payload_b64"

        if (roomLinks(decoded).isEmpty()) return "no usable room link"

        val pemBody = PUBLIC_PEM
            .replace("-----BEGIN PUBLIC KEY-----", "")
            .replace("-----END PUBLIC KEY-----", "")
            .replace("\\s".toRegex(), "")
        val key = runCatching {
            KeyFactory.getInstance("RSA")
                .generatePublic(X509EncodedKeySpec(B64.decode(pemBody)))
        }.getOrElse { return "public key parse failed" }

        val ok = runCatching {
            Signature.getInstance("SHA256withRSA").run {
                initVerify(key)
                update(payloadBytes)
                verify(signatureBytes)
            }
        }.getOrElse { return "RS256 verify error" }
        if (!ok) return "RS256 signature verification failed"

        val validUntil = decoded.optString("valid_until").ifEmpty { return "missing valid_until" }
        val until = runCatching { parseRfc3339(validUntil) }
            .getOrElse { return "valid_until parse failed" }
        if (until + 60_000 < System.currentTimeMillis()) return "manifest expired"
        return null
    }

    private fun parseRfc3339(value: String): Long {
        val fmt = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'", Locale.US)
        fmt.timeZone = TimeZone.getTimeZone("UTC")
        return fmt.parse(value)?.time ?: throw IllegalArgumentException("bad date")
    }

    fun roomLinks(payloadOrManifest: JSONObject): List<String> {
        val payload = payloadOrManifest.optJSONObject("payload") ?: payloadOrManifest
        val rooms: JSONArray = payload.optJSONArray("rooms") ?: return emptyList()
        val out = mutableListOf<String>()
        for (i in 0 until rooms.length()) {
            val url = rooms.optJSONObject(i)?.optString("url") ?: continue
            if (url.startsWith(LINK_PREFIX)) out.add(url)
        }
        return out
    }

    /**
     * Ordered candidate list for this client: FNV-1a(seed || url) ascending —
     * the same deterministic per-device spread as the desktop client.
     */
    fun selectRoomLinks(manifest: JSONObject, seed: Long): List<String> {
        val payloadBytes = runCatching {
            B64.decode(manifest.optString("payload_b64"))
        }.getOrNull() ?: return emptyList()
        val payload = runCatching { JSONObject(String(payloadBytes)) }.getOrNull() ?: return emptyList()
        return roomLinks(payload)
            .map { roomHash(seed, it) to it }
            .sortedBy { it.first }
            .map { it.second }
    }

    private fun roomHash(seed: Long, url: String): Long {
        var h = 0xcbf29ce484222325uL.toLong()
        fun feed(b: Byte) {
            h = h xor (b.toLong() and 0xFF)
            h *= 0x100000001b3L
        }
        for (shift in 0 until 8) feed((seed shr (shift * 8)).toByte())
        url.toByteArray(Charsets.UTF_8).forEach(::feed)
        return h
    }

    fun clientSeed(filesDir: File): Long {
        val f = seedFile(filesDir)
        runCatching { f.readText().trim().toLong() }.getOrNull()?.let { return it }
        val seed = SecureRandom().nextLong()
        runCatching { f.writeText(seed.toString()) }
        return seed
    }

    private fun jsonEquals(a: Any?, b: Any?): Boolean {
        return when {
            a is JSONObject && b is JSONObject -> {
                if (a.length() != b.length()) return false
                val keys = a.keys()
                while (keys.hasNext()) {
                    val k = keys.next()
                    if (!b.has(k) || !jsonEquals(a.get(k), b.get(k))) return false
                }
                true
            }
            a is JSONArray && b is JSONArray -> {
                if (a.length() != b.length()) return false
                for (i in 0 until a.length()) {
                    if (!jsonEquals(a.get(i), b.get(i))) return false
                }
                true
            }
            a == JSONObject.NULL && b == JSONObject.NULL -> true
            else -> a == b
        }
    }
}
