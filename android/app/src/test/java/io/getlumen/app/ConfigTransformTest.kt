package io.getlumen.app

import io.getlumen.app.vpn.ConfigTransform
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ConfigTransformTest {

    private fun baseConfig(): JSONObject = JSONObject(
        """
        {
          "inbounds": [
            {"type":"mixed","tag":"mixed-in","listen":"127.0.0.1","listen_port":10808}
          ],
          "outbounds": [
            {"type":"urltest","tag":"proxy-auto","outbounds":["dubai-residential","msk-via-netcup"]},
            {"type":"vless","tag":"dubai-residential","server":"198.51.100.1"},
            {"type":"vless","tag":"msk-via-netcup","server":"198.51.100.2"},
            {"type":"vless","tag":"firstbyte-tm-telemost","server":"203.0.113.5"},
            {"type":"urltest","tag":"whitelist-auto","outbounds":["firstbyte-tm-telemost"]},
            {"type":"direct","tag":"direct"}
          ],
          "route": {"rules": [], "final": "proxy-auto"},
          "telemost_manifest": {"signature_alg":"RS256"}
        }
        """.trimIndent()
    )

    private fun outbound(cfg: JSONObject, tag: String): JSONObject? {
        val arr = cfg.getJSONArray("outbounds")
        for (i in 0 until arr.length()) {
            val o = arr.getJSONObject(i)
            if (o.optString("tag") == tag) return o
        }
        return null
    }

    @Test
    fun telemostOutboundInjectedWithLocalSocks() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u1", "p1", telemostAvailable = true)
        val tl = outbound(res.config, "telemost-local")
        assertNotNull(tl)
        assertEquals("socks", tl!!.getString("type"))
        assertEquals("127.0.0.1", tl.getString("server"))
        assertEquals(ConfigTransform.TELEMOST_LOCAL_PORT, tl.getInt("server_port"))
        assertEquals("u1", tl.getString("username"))
        assertEquals("p1", tl.getString("password"))
    }

    @Test
    fun whitelistAutoGainsTelemostLocal() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = true)
        val wl = outbound(res.config, "whitelist-auto")!!.getJSONArray("outbounds")
        val members = (0 until wl.length()).map { wl.getString(it) }
        assertTrue(members.contains("telemost-local"))
        assertTrue(members.contains("firstbyte-tm-telemost"))
    }

    @Test
    fun proxyAutoNestsWhitelistAuto() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = true)
        val members = outbound(res.config, "proxy-auto")!!.getJSONArray("outbounds")
        assertTrue((0 until members.length()).any { members.getString(it) == "whitelist-auto" })
    }

    @Test
    fun manifestFieldStrippedFromConfig() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = true)
        assertFalse(res.config.has("telemost_manifest"))
    }

    @Test
    fun tunInboundRemoved() {
        val cfg = baseConfig()
        cfg.getJSONArray("inbounds").put(
            JSONObject("""{"type":"tun","tag":"tun-in"}""")
        )
        val res = ConfigTransform.apply(cfg, "/data/x/cache.db", "u", "p", telemostAvailable = false)
        val inbounds = res.config.getJSONArray("inbounds")
        for (i in 0 until inbounds.length()) {
            assertTrue(inbounds.getJSONObject(i).getString("type") != "tun")
        }
    }

    @Test
    fun noInjectionWithoutManifest() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = false)
        assertFalse(res.telemostInjected)
        assertNull(outbound(res.config, "telemost-local"))
        val wl = outbound(res.config, "whitelist-auto")!!.getJSONArray("outbounds")
        assertFalse((0 until wl.length()).any { wl.getString(it) == "telemost-local" })
    }

    @Test
    fun groupSynthesizedWhenAbsent() {
        val cfg = baseConfig()
        // Remove the server-provided whitelist-auto group.
        val arr = cfg.getJSONArray("outbounds")
        val kept = JSONArray()
        for (i in 0 until arr.length()) {
            val o = arr.getJSONObject(i)
            if (o.optString("tag") != "whitelist-auto") kept.put(o)
        }
        cfg.put("outbounds", kept)

        val res = ConfigTransform.apply(cfg, "/data/x/cache.db", "u", "p", telemostAvailable = true)
        val wl = outbound(res.config, "whitelist-auto")
        assertNotNull(wl)
        assertEquals("urltest", wl!!.getString("type"))
        val members = wl.getJSONArray("outbounds")
        assertTrue((0 until members.length()).any { members.getString(it) == "telemost-local" })
    }

    // ---- legacy sing-box 1.14 migration ----

    @Test
    fun legacyDnsServersMigrated() {
        val cfg = baseConfig()
        cfg.put("dns", JSONObject("""{"strategy":"ipv4_only","servers":[
            {"tag":"dns-proxy","address":"https://dns.cloudflare.com/dns-query","address_resolver":"dns-direct","detour":"proxy-auto"},
            {"tag":"dns-direct","address":"8.8.8.8","detour":"direct"},
            {"tag":"dns-block","address":"rcode://success"}
        ],"rules":[{"domain":["api.oneme.ru"],"server":"dns-block"}],"final":"dns-proxy"}"""))
        val res = ConfigTransform.apply(cfg, "/data/x/cache.db", "u", "p", telemostAvailable = false)
        val servers = res.config.getJSONObject("dns").getJSONArray("servers")

        fun byTag(t: String): JSONObject? = (0 until servers.length())
            .map { servers.getJSONObject(it) }
            .firstOrNull { it.optString("tag") == t }

        val proxy = byTag("dns-proxy")!!
        assertEquals("https", proxy.getString("type"))
        assertEquals("dns.cloudflare.com", proxy.getString("server"))
        assertFalse(proxy.has("path")) // default /dns-query path dropped
        assertFalse(proxy.has("address"))
        assertEquals("dns-direct", proxy.getString("domain_resolver"))

        val direct = byTag("dns-direct")!!
        assertEquals("udp", direct.getString("type"))
        assertEquals("8.8.8.8", direct.getString("server"))

        // rcode server folded away + rule rewritten to action:predefined
        assertNull(byTag("dns-block"))
        val rule = res.config.getJSONObject("dns").getJSONArray("rules").getJSONObject(0)
        assertEquals("predefined", rule.getString("action"))
        assertEquals("NOERROR", rule.getString("rcode"))
        assertFalse(rule.has("server"))

        // A `local` resolver server was added and `direct` dials point at it.
        val local = byTag("local") ?: byTag("local-system")
        assertNotNull(local)
        val directOutbound = outbound(res.config, "direct")!!
        assertEquals(local!!.getString("tag"), directOutbound.getString("domain_resolver"))
    }

    @Test
    fun conditionlessRouteRuleDropped() {
        val cfg = baseConfig()
        cfg.getJSONObject("route").put("rules", JSONArray("""[
            {"domain_suffix":[],"outbound":"msk-via-netcup"},
            {"action":"sniff"},
            {"domain":"keep.me","outbound":"direct"}
        ]"""))
        val res = ConfigTransform.apply(cfg, "/data/x/cache.db", "u", "p", telemostAvailable = false)
        val rules = res.config.getJSONObject("route").getJSONArray("rules")
        val json = rules.toString()
        assertFalse(json.contains("msk-via-netcup"))
        assertTrue(json.contains("sniff"))
        assertTrue(json.contains("keep.me"))
    }

    @Test
    fun inboundSniffFieldsStripped() {
        val cfg = baseConfig()
        cfg.getJSONArray("inbounds").getJSONObject(0)
            .put("sniff", true).put("sniff_timeout", "300ms")
        val res = ConfigTransform.apply(cfg, "/data/x/cache.db", "u", "p", telemostAvailable = false)
        val inbound = res.config.getJSONArray("inbounds").getJSONObject(0)
        assertFalse(inbound.has("sniff"))
        assertFalse(inbound.has("sniff_timeout"))
    }

    @Test
    fun cacheFilePinnedAbsolute() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = false)
        val path = res.config.getJSONObject("experimental")
            .getJSONObject("cache_file").getString("path")
        assertEquals("/data/x/cache.db", path)
    }

    /**
     * Local verification hook: transform a real fetched config and write it
     * out for `sing-box check`. Skips unless SMOKE_CONFIG + SMOKE_OUT are set.
     */
    @Test
    fun smokeRealConfig() {
        val src = System.getenv("SMOKE_CONFIG") ?: return
        val out = System.getenv("SMOKE_OUT") ?: return
        val raw = JSONObject(java.io.File(src).readText())
        val res = ConfigTransform.apply(raw, "/data/x/cache.db", "u", "p", telemostAvailable = true)
        java.io.File(out).writeText(res.config.toString(2))
        assertTrue(res.telemostInjected)
    }

    @Test
    fun telemostDomainsPinnedDirect() {
        val res = ConfigTransform.apply(baseConfig(), "/data/x/cache.db", "u", "p", telemostAvailable = true)
        val rules = res.config.getJSONObject("route").getJSONArray("rules")
        val first = rules.getJSONObject(0)
        assertEquals("direct", first.getString("outbound"))
        val suffixes = first.getJSONArray("domain_suffix")
        assertTrue((0 until suffixes.length()).any { suffixes.getString(it) == "telemost.yandex.ru" })
    }
}
