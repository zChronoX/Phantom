package app.phantom.host.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
        assertEquals(1088 to 2400, HostFormatHelpers.scaledDimensions(1080, 2400, 1080))
        assertEquals(720 to 1600, HostFormatHelpers.scaledDimensions(1080, 2400, 720))
        assertEquals(1600 to 720, HostFormatHelpers.scaledDimensions(2400, 1080, 720))
        assertEquals("00ff10", HostFormatHelpers.toHex(byteArrayOf(0x00, 0xFF.toByte(), 0x10)))
    }
}
