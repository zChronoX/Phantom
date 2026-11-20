package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AudioSampleClockTest {
    @Test
    fun timestampsFollowSampleCountDespiteReadJitter() {
        val clock = AudioSampleClock(48_000)
        val first = clock.onChunk(1024, 1_021_333L)
        assertEquals(1_000_000L, first)
        // read() che ritorna con qualche ms di anticipo o ritardo: l'istante resta quello dei campioni
        assertEquals(1_021_333L, clock.onChunk(1024, 1_045_000L))
        assertEquals(1_042_666L, clock.onChunk(1024, 1_060_000L))
        assertEquals(1_064_000L, clock.onChunk(1024, 1_087_000L))
        assertEquals(0, clock.rebases)
    }

    @Test
    fun rebasesWhenCaptureStalls() {
        val clock = AudioSampleClock(48_000)
        clock.onChunk(1024, 1_021_333L)
        // la cattura si ferma mezzo secondo: si riparte dall'ora attuale
        val pts = clock.onChunk(1024, 1_600_000L)
        assertEquals(1_600_000L - 21_333L, pts)
        assertEquals(1, clock.rebases)
        assertTrue(clock.onChunk(1024, 1_621_000L) > pts)
    }
}
