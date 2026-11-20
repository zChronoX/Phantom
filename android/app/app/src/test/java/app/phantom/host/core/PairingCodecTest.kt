package app.phantom.host.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class PairingCodecTest {
    @Test
    fun sha256FingerprintMatchesStandardVector() {
        val fp = PairingCodec.sha256FingerprintHex("abc".toByteArray(Charsets.UTF_8), 16)
        assertEquals("ba7816bf8f01cfea", fp)
    }

    @Test
    fun base64UrlRoundtripsAllPaddingLengths() {
        for (len in 0..15) {
            val raw = ByteArray(len) { ((it * 37 + 11) and 0xFF).toByte() }
            val encoded = PairingCodec.encodeBase64UrlNoPad(raw)
            val decoded = PairingCodec.decodeBase64UrlNoPad(encoded)
            assertNotNull("len=$len", decoded)
            assertArrayEquals("len=$len", raw, decoded)
        }
    }

    @Test
    fun pairingUriRoundtripsLanWanNameAndCert() {
        val cert = byteArrayOf(0x30, 0x82.toByte(), 0x01, 0x0A, 0x05, 0xFF.toByte(), 0x7E)
        val tok = ByteArray(16) { (it + 1).toByte() }
        val uri = PairingCodec.buildPairingUri(
            name = "Redmi Note 11 Pro",
            lan = "192.168.1.42:47800",
            wan = "100.88.12.5:47800",
            certDer = cert,
            p2pId = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            authToken = tok,
        )
        val parsed = PairingCodec.parsePairingUri(uri)
        assertNotNull(parsed)
        assertEquals("Redmi Note 11 Pro", parsed!!.name)
        assertEquals("192.168.1.42:47800", parsed.lan)
        assertEquals("100.88.12.5:47800", parsed.wan)
        assertEquals("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789", parsed.p2pId)
        assertArrayEquals(tok, parsed.authToken)
        assertArrayEquals(cert, parsed.certDer)

        assertNull(PairingCodec.parsePairingUri("https://example.com"))
        assertNull(PairingCodec.parsePairingUri("phantom://pair?name=test"))
    }

    @Test
    fun selectsVpnAndTailscaleIpv4Correctly() {
        assertEquals(
            "100.88.12.5",
            HostFormatHelpers.selectPreferredVpnIpv4(
                listOf("wlan0" to "192.168.1.42", "tun0" to "100.88.12.5"),
            ),
        )
        assertEquals(
            "10.8.0.2",
            HostFormatHelpers.selectPreferredVpnIpv4(
                listOf("wlan0" to "192.168.1.42", "wg0" to "10.8.0.2"),
            ),
        )
        assertEquals(
            "",
            HostFormatHelpers.selectPreferredVpnIpv4(
                listOf("wlan0" to "192.168.1.42", "eth0" to "172.16.1.15"),
            ),
        )
    }
}
