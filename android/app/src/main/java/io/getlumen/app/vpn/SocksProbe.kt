package io.getlumen.app.vpn

import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetSocketAddress
import java.net.Socket

/**
 * Data-plane probe through a SOCKS5 server: greeting, CONNECT, then a real
 * HTTP request that must return at least one response byte. A granted CONNECT
 * alone is not proof — a dead tunnel can still ACK control frames while
 * response data never comes back.
 */
object SocksProbe {

    fun probe(
        socksPort: Int,
        host: String = "www.gstatic.com",
        port: Int = 80,
        timeoutMs: Int = 8_000,
        user: String? = null,
        pass: String? = null,
    ): Boolean = runCatching {
        Socket().use { socket ->
            socket.tcpNoDelay = true
            socket.soTimeout = timeoutMs
            socket.connect(InetSocketAddress("127.0.0.1", socksPort), timeoutMs)
            val input = DataInputStream(socket.getInputStream())
            val output = DataOutputStream(socket.getOutputStream())

            val hasAuth = !user.isNullOrEmpty()
            output.writeByte(0x05)
            output.writeByte(if (hasAuth) 2 else 1)
            output.writeByte(0x00)
            if (hasAuth) output.writeByte(0x02)
            output.flush()

            val method = input.readByte()
            val chosen = input.readByte()
            if (method.toInt() != 0x05 || chosen.toInt() == 0xFF) return@runCatching false

            if (chosen.toInt() == 0x02) {
                val u = user!!.toByteArray(Charsets.UTF_8)
                val p = (pass ?: "").toByteArray(Charsets.UTF_8)
                output.writeByte(0x01)
                output.writeByte(u.size)
                output.write(u)
                output.writeByte(p.size)
                output.write(p)
                output.flush()
                input.readByte() // version
                if (input.readByte().toInt() != 0x00) return@runCatching false
            }

            // CONNECT host:port (ATYP=domain)
            val hostBytes = host.toByteArray(Charsets.US_ASCII)
            output.writeByte(0x05); output.writeByte(0x01); output.writeByte(0x00)
            output.writeByte(0x03); output.writeByte(hostBytes.size)
            output.write(hostBytes)
            output.writeByte(port shr 8); output.writeByte(port and 0xFF)
            output.flush()

            val head = ByteArray(4)
            input.readFully(head)
            if (head[1].toInt() != 0x00) return@runCatching false

            // Drain BND.ADDR per ATYP.
            when (head[3].toInt()) {
                0x01 -> input.skipFully(4 + 2)
                0x04 -> input.skipFully(16 + 2)
                0x03 -> {
                    val len = input.readByte().toInt()
                    input.skipFully(len + 2)
                }
                else -> return@runCatching false
            }

            output.write(
                "GET /generate_204 HTTP/1.1\r\nHost: $host\r\nConnection: close\r\n\r\n"
                    .toByteArray(Charsets.US_ASCII)
            )
            output.flush()

            // One response byte is enough proof the downlink carries data.
            input.readByte()
            true
        }
    }.getOrDefault(false)

    private fun DataInputStream.skipFully(n: Int) {
        var left = n
        while (left > 0) {
            val skipped = skipBytes(left)
            if (skipped <= 0) throw java.io.EOFException()
            left -= skipped
        }
    }
}
