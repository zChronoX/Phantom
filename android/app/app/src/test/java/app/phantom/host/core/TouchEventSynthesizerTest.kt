package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TouchEventSynthesizerTest {
    @Test
    fun singleFingerLifecycleDownMoveUp() {
        val synth = TouchEventSynthesizer()

        val down = synth.update(listOf(NormalizedInputFinger(0, 0, 65535, 128)), 1080, 2400)
        assertEquals(1, down.size)
        assertEquals(SynthAction.DOWN, down[0].action)
        assertEquals(0, down[0].actionIndex)
        assertEquals(0f, down[0].pointers[0].x, 0.01f)
        assertEquals(2399f, down[0].pointers[0].y, 0.01f)

        val move = synth.update(listOf(NormalizedInputFinger(0, 32768, 32768, 255)), 1080, 2400)
        assertEquals(1, move.size)
        assertEquals(SynthAction.MOVE, move[0].action)
        assertEquals(539.5f, move[0].pointers[0].x, 0.5f)
        assertEquals(1.0f, move[0].pointers[0].pressure, 0.01f)

        // Stato identico senza spostamenti -> nessun evento ridondante
        assertTrue(synth.update(listOf(NormalizedInputFinger(0, 32768, 32768, 255)), 1080, 2400).isEmpty())

        val up = synth.update(emptyList(), 1080, 2400)
        assertEquals(1, up.size)
        assertEquals(SynthAction.UP, up[0].action)
        assertEquals(1, up[0].pointers.size)
    }

    @Test
    fun multiTouchEmitsPointerDownAndPointerUpWithCorrectIndices() {
        val synth = TouchEventSynthesizer()

        // Dito 0 scende -> DOWN; poi dito 1 scende insieme -> POINTER_DOWN con 2 dita
        val s1 = synth.update(listOf(NormalizedInputFinger(0, 10000, 10000, 100)), 1080, 2400)
        assertEquals(listOf(SynthAction.DOWN), s1.map { it.action })

        val s2 = synth.update(
            listOf(
                NormalizedInputFinger(0, 10000, 10000, 100),
                NormalizedInputFinger(1, 50000, 50000, 100),
            ),
            1080,
            2400,
        )
        assertEquals(1, s2.size)
        assertEquals(SynthAction.POINTER_DOWN, s2[0].action)
        assertEquals(1, s2[0].actionIndex)
        assertEquals(2, s2[0].pointers.size)

        // Rilascio simultaneo di entrambe le dita -> POINTER_UP poi UP
        val s3 = synth.update(emptyList(), 1080, 2400)
        assertEquals(listOf(SynthAction.POINTER_UP, SynthAction.UP), s3.map { it.action })
        assertEquals(2, s3[0].pointers.size)
        assertEquals(1, s3[1].pointers.size)
    }
}
