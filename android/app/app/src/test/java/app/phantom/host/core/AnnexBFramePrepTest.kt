package app.phantom.host.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AnnexBFramePrepTest {
    @Test
    fun cachesConfigAndPrependsOnlyToKeyFramesWhenMissing() {
        val cache = CodecConfigCache()
        val cfg = byteArrayOf(0, 0, 0, 1, 0x40, 0x01)
        val idr = byteArrayOf(0, 0, 0, 1, 0x26, 0xAA.toByte())
        val delta = byteArrayOf(0, 0, 0, 1, 0x02, 0x11)

        assertNull(cache.prepareFrame(cfg, isConfig = true, isKeyFrame = false))

        val combined = cache.prepareFrame(idr, isConfig = false, isKeyFrame = true)!!
        assertArrayEquals(cfg + idr, combined)

        // Se il keyframe contiene già i parametri all'inizio non va duplicato
        val alreadyCombined = cache.prepareFrame(cfg + idr, isConfig = false, isKeyFrame = true)!!
        assertArrayEquals(cfg + idr, alreadyCombined)

        val outDelta = cache.prepareFrame(delta, isConfig = false, isKeyFrame = false)!!
        assertArrayEquals(delta, outDelta)
    }

    @Test
    fun formatHelpersScaleAndFormatHexCorrectly() {
        assertEquals(1088, HostFormatHelpers.align16(1080))
        // Niente più arrotondamento per eccesso: 1080 resta 1080 (prima diventava 1088 e l'immagine si stirava)
        assertEquals(1080 to 2400, HostFormatHelpers.scaledDimensions(1080, 2400, 1080))
        assertEquals(720 to 1600, HostFormatHelpers.scaledDimensions(1080, 2400, 720))
        assertEquals(1600 to 720, HostFormatHelpers.scaledDimensions(2400, 1080, 720))
        // Snapdragon 695 / encoder limitati a 1920 sul lato lungo (display 20:9 = 1088×2400)
        assertEquals(864 to 1920, HostFormatHelpers.fitWithinBounds(1088, 2400, 1920, 1920))
        assertEquals(1920 to 864, HostFormatHelpers.fitWithinBounds(2400, 1088, 1920, 1920))
        assertEquals(1088 to 1920, HostFormatHelpers.fitWithinBounds(1088, 1920, 1920, 1920))
        assertEquals("00ff10", HostFormatHelpers.toHex(byteArrayOf(0x00, 0xFF.toByte(), 0x10)))
    }

    @Test
    fun encoderSizeKeepsAspectAndUsesExactScaleWhenPossible() {
        // Redmi Note 11 Pro 5G (1080×2400) con encoder che accetta tutto: nessuna riduzione
        assertEquals(1080 to 2400, HostFormatHelpers.fitEncoderSize(1080, 2400, 4096, 4096, 8))
        // Limite 1920 sul lato lungo: scala esatta 0,8 → 864×1920, proporzioni identiche
        assertEquals(864 to 1920, HostFormatHelpers.fitEncoderSize(1080, 2400, 1920, 1920, 16))
        assertEquals(1920 to 864, HostFormatHelpers.fitEncoderSize(2400, 1080, 1920, 1920, 16))
        // Mai ingrandire
        assertEquals(720 to 1600, HostFormatHelpers.fitEncoderSize(720, 1600, 4096, 4096, 8))
        // Limite sui macroblocchi totali: si scende a piccoli passi, non del 10% alla volta
        val (w, h) = HostFormatHelpers.fitEncoderSize(1080, 2400, 4096, 4096, 8) { cw, ch -> cw * ch <= 1920 * 1080 }
        assertTrue(w * h <= 1920 * 1080)
        assertTrue("troppa risoluzione persa: ${w}×$h", w * h >= (1920 * 1080 * 0.90).toInt())
        assertEquals(0, w % 8)
        assertEquals(0, h % 8)
        val aspect = w.toDouble() / h
        assertTrue(kotlin.math.abs(aspect - 0.45) < 0.01)
    }

    @Test
    fun usefulBitrateScalesWithPixels() {
        // 864×1920 a 60 fps in HEVC: ~15 Mbit/s bastano
        assertEquals(14_929_920L, HostFormatHelpers.maxUsefulBitrate(864, 1920, 60, hevc = true))
        // 488×1088: ~4,8 Mbit/s (prima si arrivava a 33)
        assertTrue(HostFormatHelpers.maxUsefulBitrate(488, 1088, 60, hevc = true) < 5_000_000L)
        // H.264 ha bisogno di più bit
        assertTrue(HostFormatHelpers.maxUsefulBitrate(864, 1920, 60, hevc = false) > 20_000_000L)
        // mai sotto 1 Mbit/s
        assertEquals(1_000_000L, HostFormatHelpers.maxUsefulBitrate(100, 100, 30, hevc = true))
    }

    @Test
    fun realisticFpsFollowsDeclaredPerformance() {
        // SoC che dichiara solo 60 fps a questa risoluzione: 120 → 60
        assertEquals(60, HostFormatHelpers.realisticFps(120) { fps -> fps <= 60 })
        // SoC che regge 90 ma non 120
        assertEquals(90, HostFormatHelpers.realisticFps(120) { fps -> fps <= 90 })
        // SoC che regge 120: resta 120
        assertEquals(120, HostFormatHelpers.realisticFps(120) { true })
        // SoC che non dichiara nulla: si tiene la richiesta
        assertEquals(120, HostFormatHelpers.realisticFps(120) { null })
        // Richiesta già bassa
        assertEquals(60, HostFormatHelpers.realisticFps(60) { fps -> fps <= 60 })
    }

    @Test
    fun captureTimeConvertsMonotonicPtsToElapsedClock() {
        // Frame catturato 12 ms fa sull'orologio monotono
        assertEquals(5_000_000L - 12_000L, HostFormatHelpers.captureTimeUs(988_000L, 1_000_000L, 5_000_000L))
        // Valori assurdi: si usa l'istante attuale
        assertEquals(5_000_000L, HostFormatHelpers.captureTimeUs(0L, 1_000_000L, 5_000_000L))
        assertEquals(5_000_000L, HostFormatHelpers.captureTimeUs(2_000_000L, 1_000_000L, 5_000_000L))
        assertEquals(5_000_000L, HostFormatHelpers.captureTimeUs(1L, 9_000_000L, 5_000_000L))
    }

    @Test
    fun selectsLanIpv4AcrossWifiAndBridgedEthernet() {
        // Telefono reale: dati mobili + Wi-Fi
        assertEquals(
            "192.168.1.42",
            HostFormatHelpers.selectPreferredLanIpv4(
                listOf("rmnet0" to "100.64.0.10", "wlan0" to "192.168.1.42"),
            ),
        )
        // LDPlayer in Network Bridging: NAT 10.0.2.15 / 172.16.1.15 + bridge LAN
        assertEquals(
            "192.168.1.55",
            HostFormatHelpers.selectPreferredLanIpv4(
                listOf("eth0" to "10.0.2.15", "eth1" to "192.168.1.55"),
            ),
        )
        assertEquals(
            "192.168.1.77",
            HostFormatHelpers.selectPreferredLanIpv4(
                listOf("eth0" to "172.16.1.15", "eth1" to "192.168.1.77"),
            ),
        )
        // Solo link-local vs Ethernet LAN
        assertEquals(
            "192.168.1.80",
            HostFormatHelpers.selectPreferredLanIpv4(
                listOf("wlan0" to "169.254.1.2", "eth0" to "192.168.1.80"),
            ),
        )
        assertEquals("", HostFormatHelpers.selectPreferredLanIpv4(emptyList()))
    }
}
