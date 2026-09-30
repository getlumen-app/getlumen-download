package io.getlumen.app.vpn

import org.json.JSONArray
import org.json.JSONObject

/**
 * Android-side transform of the server sing-box config — a port of the
 * desktop `migrate_legacy_singbox_config` + `inject_telemost_fallback`:
 *
 * - legacy (pre-1.12) DNS servers `{"address": ...}` -> typed 1.14 servers;
 *   `rcode://X` servers fold into `action: predefined` DNS rules
 * - `address_resolver` -> `domain_resolver`; direct outbounds and dial
 *   fields get an explicit resolver
 * - strips client-side metadata, unusable `tun` inbounds and removed
 *   inbound sniff fields; drops conditionless route rules
 * - pins `experimental.cache_file.path` to an absolute app path
 * - injects `telemost-local` (SOCKS to the local joiner) into
 *   `whitelist-auto`, and `whitelist-auto` into `proxy-auto`/`proxy`.
 *
 * The urltest does failover in both directions — the joiner only wins while
 * foreign exits probe dead — and `interrupt_exist_connections: false` keeps
 * live streams alive across switches.
 */
object ConfigTransform {

    const val TELEMOST_LOCAL_PORT = 11097
    const val MIXED_PORT = 10808
    private const val TELEMOST_LOCAL_TAG = "telemost-local"
    private val TELEMOST_RELAY_TAGS = listOf("izhevsk-telemost", "firstbyte-tm-telemost")
    private val TELEMOST_DOMAINS = listOf("telemost.yandex.ru", ".strm.yandex.net", "yastatic.net")

    private val ROUTE_CONDITION_KEYS = listOf(
        "domain", "domain_suffix", "domain_keyword", "domain_regex",
        "ip_cidr", "rule_set", "geoip", "geosite", "network", "port",
        "port_range", "protocol", "ip_version", "ip_is_private",
        "source_ip_cidr", "source_port", "user", "process_name",
        "package_name", "wifi_ssid", "wifi_bssid", "clash_mode", "client",
    )

    class TransformResult(
        val config: JSONObject,
        val telemostInjected: Boolean,
        val notes: List<String>,
    )

    fun apply(
        raw: JSONObject,
        cacheFilePath: String,
        telemostUser: String?,
        telemostPass: String?,
        telemostAvailable: Boolean,
    ): TransformResult {
        val notes = mutableListOf<String>()
        val config = JSONObject(raw.toString()) // deep copy

        // Client-side metadata is not part of the sing-box schema.
        config.remove("telemost_manifest")
        config.remove("wbstream_manifest")

        migrateLegacyDnsServers(config, notes)
        ensureLocalDnsServer(config)
        ensureDialResolvers(config)
        stripLegacyInboundFields(config)
        dropConditionlessRouteRules(config)
        stripTunInbounds(config, notes)
        ensureMixedInbound(config)

        // auto_detect_interface makes sing-box subscribe to netlink route
        // updates, which needs CAP_NET_ADMIN — always denied inside an app
        // sandbox. The VpnService builder owns interface routing anyway.
        config.optJSONObject("route")?.remove("auto_detect_interface")

        // cache_file relative paths resolve against the spawned process cwd —
        // pin it under the app files dir.
        val experimental = config.optJSONObject("experimental")
            ?: JSONObject().also { config.put("experimental", it) }
        val cacheFile = experimental.optJSONObject("cache_file")
            ?: JSONObject().also { experimental.put("cache_file", it) }
        cacheFile.put("enabled", true)
        cacheFile.put("path", cacheFilePath)

        var injected = false
        if (telemostAvailable) {
            injected = injectTelemost(config, telemostUser ?: "", telemostPass ?: "", notes)
        }
        return TransformResult(config, injected, notes)
    }

    // ---- legacy DNS migration (parity with migrate_legacy_dns_servers) ----

    private fun migrateLegacyDnsServers(config: JSONObject, notes: MutableList<String>) {
        val servers = config.optJSONObject("dns")?.optJSONArray("servers") ?: return
        val rcode = mutableMapOf<String, String>()

        val kept = JSONArray()
        for (i in 0 until servers.length()) {
            val obj = servers.optJSONObject(i) ?: continue
            obj.remove("strategy")
            obj.remove("client_subnet")
            if (obj.has("address_resolver") && !obj.has("domain_resolver")) {
                obj.put("domain_resolver", obj.remove("address_resolver"))
            } else {
                obj.remove("address_resolver")
            }
            val addr = obj.optString("address")
            if (addr.isEmpty()) {
                kept.put(obj) // already typed
                continue
            }
            obj.remove("address")
            if (addr.startsWith("rcode://")) {
                // sing-box names rcode 0 NOERROR; legacy configs write "success".
                val name = addr.removePrefix("rcode://").uppercase()
                val tag = obj.optString("tag")
                if (tag.isNotEmpty()) rcode[tag] = if (name == "SUCCESS") "NOERROR" else name
                continue // rcode servers no longer exist in 1.14
            }
            val (typ, server, path, port) = splitLegacyDnsAddress(addr, notes)
            obj.put("type", typ)
            if (server != null) obj.put("server", server)
            if (path != null) obj.put("path", path)
            if (port != null) obj.put("server_port", port)
            kept.put(obj)
        }
        config.getJSONObject("dns").put("servers", kept)

        if (rcode.isNotEmpty()) rewriteRcodeDnsRules(config, rcode)
    }

    /** `(type, server, path, server_port)` for a legacy `address` value. */
    private fun splitLegacyDnsAddress(
        addr: String,
        notes: MutableList<String>,
    ): Quadruple<String, String?, String?, Int?> {
        if (addr == "local") return Quadruple("local", null, null, null)
        if (addr == "fakeip") return Quadruple("fakeip", null, null, null)

        val schemeSep = addr.indexOf("://")
        val scheme: String
        val rest: String
        if (schemeSep >= 0) {
            scheme = addr.substring(0, schemeSep)
            rest = addr.substring(schemeSep + 3)
        } else {
            scheme = "udp"
            rest = addr
        }
        if (scheme == "dhcp") return Quadruple("dhcp", null, null, null)

        val typ = when (scheme) {
            "https", "h3", "tls", "quic", "tcp", "udp" -> scheme
            else -> {
                notes.add("dns server address '$addr': unknown scheme, assuming udp")
                "udp"
            }
        }
        val slash = rest.indexOf('/')
        val hostport = if (slash >= 0) rest.substring(0, slash) else rest
        val rawPath = if (slash >= 0) rest.substring(slash) else null

        // Don't split IPv6 literals like [2001:db8::1].
        var host = hostport
        var port: Int? = null
        val colon = hostport.lastIndexOf(':')
        if (colon >= 0 && !hostport.substring(0, colon).endsWith("[")) {
            host = hostport.substring(0, colon)
            port = hostport.substring(colon + 1).toIntOrNull()
        }
        return Quadruple(typ, host, rawPath?.takeIf { it != "/dns-query" }, port)
    }

    private data class Quadruple<A, B, C, D>(val a: A, val b: B, val c: C, val d: D)

    private fun rewriteRcodeDnsRules(config: JSONObject, rcode: Map<String, String>) {
        val rules = config.optJSONObject("dns")?.optJSONArray("rules") ?: return
        for (i in 0 until rules.length()) {
            val rule = rules.optJSONObject(i) ?: continue
            val tag = rule.optString("server")
            val rc = rcode[tag] ?: continue
            rule.remove("server")
            rule.put("action", "predefined")
            rule.put("rcode", rc)
        }
    }

    private fun ensureLocalDnsServer(config: JSONObject): String {
        val dns = config.optJSONObject("dns") ?: JSONObject().also { config.put("dns", it) }
        val servers = dns.optJSONArray("servers") ?: JSONArray().also { dns.put("servers", it) }
        for (i in 0 until servers.length()) {
            val s = servers.optJSONObject(i) ?: continue
            if (s.optString("type") == "local") {
                return s.optString("tag").ifEmpty { "local" }
            }
        }
        val taken = mutableSetOf<String>()
        for (i in 0 until servers.length()) {
            servers.optJSONObject(i)?.optString("tag")?.let { taken.add(it) }
        }
        val tag = if ("local" !in taken) "local" else "local-system"
        servers.put(JSONObject().put("tag", tag).put("type", "local"))
        return tag
    }

    private fun ensureDialResolvers(config: JSONObject) {
        val localTag = ensureLocalDnsServer(config)
        config.optJSONArray("outbounds")?.let { arr ->
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                if (o.optString("type") == "direct" && !o.has("domain_resolver")) {
                    o.put("domain_resolver", localTag)
                }
            }
        }
        val route = config.optJSONObject("route") ?: JSONObject().also { config.put("route", it) }
        if (!route.has("default_domain_resolver")) {
            val prefer = config.optJSONObject("dns")?.optJSONArray("servers")?.let { arr ->
                (0 until arr.length()).any {
                    arr.optJSONObject(it)?.optString("tag") == "dns-proxy"
                }.let { found -> if (found) "dns-proxy" else localTag }
            } ?: localTag
            route.put("default_domain_resolver", prefer)
        }
    }

    /** Inbound sniff fields were removed in sing-box 1.13. */
    private fun stripLegacyInboundFields(config: JSONObject) {
        val inbounds = config.optJSONArray("inbounds") ?: return
        for (i in 0 until inbounds.length()) {
            val obj = inbounds.optJSONObject(i) ?: continue
            obj.remove("sniff")
            obj.remove("sniff_override_destination")
            obj.remove("sniff_timeout")
        }
    }

    /**
     * Drop route rules whose only "condition" is empty — a condition-less
     * rule matches ALL traffic and can silently reroute the final.
     */
    private fun dropConditionlessRouteRules(config: JSONObject) {
        val rules = config.optJSONObject("route")?.optJSONArray("rules") ?: return
        val kept = JSONArray()
        for (i in 0 until rules.length()) {
            val rule = rules.optJSONObject(i) ?: continue
            if (rule.has("action")) {
                kept.put(rule)
                continue
            }
            val hasCondition = ROUTE_CONDITION_KEYS.any { k ->
                when (val v = rule.opt(k)) {
                    is JSONArray -> v.length() > 0
                    null, JSONObject.NULL -> false
                    else -> true
                }
            }
            if (hasCondition) kept.put(rule)
        }
        config.getJSONObject("route").put("rules", kept)
    }

    /** Android gets TUN through VpnService + tun2socks — strip tun inbounds. */
    private fun stripTunInbounds(config: JSONObject, notes: MutableList<String>) {
        val inbounds = config.optJSONArray("inbounds") ?: return
        val kept = JSONArray()
        for (i in 0 until inbounds.length()) {
            val inbound = inbounds.optJSONObject(i) ?: continue
            if (inbound.optString("type") == "tun") {
                notes.add("removed tun inbound (Android uses VpnService)")
            } else {
                kept.put(inbound)
            }
        }
        config.put("inbounds", kept)
    }

    private fun ensureMixedInbound(config: JSONObject) {
        val inbounds = config.optJSONArray("inbounds") ?: JSONArray()
        for (i in 0 until inbounds.length()) {
            if (inbounds.optJSONObject(i)?.optString("type") == "mixed") return
        }
        inbounds.put(JSONObject().apply {
            put("type", "mixed")
            put("tag", "mixed-in")
            put("listen", "127.0.0.1")
            put("listen_port", MIXED_PORT)
        })
        config.put("inbounds", inbounds)
    }

    // ---- Telemost fallback injection (parity with inject_telemost_fallback) ----

    private fun injectTelemost(
        config: JSONObject,
        user: String,
        pass: String,
        notes: MutableList<String>,
    ): Boolean {
        val outbounds = config.optJSONArray("outbounds") ?: return false

        if (!hasTag(outbounds, TELEMOST_LOCAL_TAG)) {
            outbounds.put(JSONObject().apply {
                put("type", "socks")
                put("tag", TELEMOST_LOCAL_TAG)
                put("server", "127.0.0.1")
                put("server_port", TELEMOST_LOCAL_PORT)
                put("version", "5")
                put("username", user)
                put("password", pass)
            })
        }

        // whitelist-auto members: local joiner + server-provided relay leaves.
        val wlMembers = mutableListOf(TELEMOST_LOCAL_TAG)
        for (tag in TELEMOST_RELAY_TAGS) {
            if (hasLeaf(outbounds, tag)) wlMembers.add(tag)
        }

        var groupExists = false
        forEachGroup(outbounds) { o ->
            if (o.optString("tag") == "whitelist-auto") {
                groupExists = true
                val list = o.optJSONArray("outbounds") ?: JSONArray().also { o.put("outbounds", it) }
                for (m in wlMembers) if (!contains(list, m)) list.put(m)
            }
        }
        if (!groupExists) {
            outbounds.put(JSONObject().apply {
                put("type", "urltest")
                put("tag", "whitelist-auto")
                put("outbounds", JSONArray(wlMembers))
                put("url", "https://www.cloudflare.com/cdn-cgi/trace")
                put("interval", "15s")
                put("tolerance", 200)
                put("idle_timeout", "30m")
                put("interrupt_exist_connections", false)
            })
            notes.add("synthesized whitelist-auto group")
        }

        forEachGroup(outbounds) { o ->
            val tag = o.optString("tag")
            if (tag == "proxy-auto" || tag == "proxy") {
                val list = o.optJSONArray("outbounds") ?: return@forEachGroup
                if (!contains(list, "whitelist-auto")) list.put("whitelist-auto")
            }
        }

        // The joiner binary's own signalling/media traffic must never loop
        // back through the tunnel. The app package is already disallowed from
        // the VpnService builder, so these rules are belt-and-suspenders for
        // any future packet that still resolves Telemost domains.
        val rules = config.optJSONObject("route")
            ?.optJSONArray("rules") ?: JSONArray().also {
            config.getJSONObject("route").put("rules", it)
        }
        val directRule = JSONObject().apply {
            put("domain_suffix", JSONArray(TELEMOST_DOMAINS))
            put("outbound", "direct")
        }
        rules.put(0, directRule)

        return true
    }

    private fun hasTag(outbounds: JSONArray, tag: String): Boolean {
        for (i in 0 until outbounds.length()) {
            if (outbounds.optJSONObject(i)?.optString("tag") == tag) return true
        }
        return false
    }

    /** True when a non-group outbound with this tag exists. */
    private fun hasLeaf(outbounds: JSONArray, tag: String): Boolean {
        for (i in 0 until outbounds.length()) {
            val o = outbounds.optJSONObject(i) ?: continue
            val type = o.optString("type")
            if (o.optString("tag") == tag && type != "urltest" && type != "selector") return true
        }
        return false
    }

    private inline fun forEachGroup(outbounds: JSONArray, block: (JSONObject) -> Unit) {
        for (i in 0 until outbounds.length()) {
            val o = outbounds.optJSONObject(i) ?: continue
            val type = o.optString("type")
            if (type == "urltest" || type == "selector") block(o)
        }
    }

    private fun contains(array: JSONArray, value: String): Boolean {
        for (i in 0 until array.length()) {
            if (array.optString(i) == value) return true
        }
        return false
    }
}
