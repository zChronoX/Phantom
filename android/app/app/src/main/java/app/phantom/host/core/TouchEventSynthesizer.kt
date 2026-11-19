package app.phantom.host.core

data class NormalizedInputFinger(
    val id: Int,
    val x: Int,
    val y: Int,
    val pressure: Int,
)

data class SynthFinger(
    val id: Int,
    val x: Float,
    val y: Float,
    val pressure: Float,
)

enum class SynthAction {
    DOWN,
    POINTER_DOWN,
    MOVE,
    POINTER_UP,
    UP,
}

data class SynthMotionStep(
    val action: SynthAction,
    val actionIndex: Int,
    val pointers: List<SynthFinger>,
)

/**
 * Converte lo stato completo delle dita ricevuto dal protocollo Phantom nella sequenza esatta di
 * eventi `DOWN` / `POINTER_DOWN` / `MOVE` / `POINTER_UP` / `UP` richiesta da Android `InputManager`.
 */
class TouchEventSynthesizer {
    private val active = ArrayList<SynthFinger>(10)

    fun reset() {
        active.clear()
    }

    fun update(fingers: List<NormalizedInputFinger>, width: Int, height: Int): List<SynthMotionStep> {
        val maxX = (width - 1).coerceAtLeast(1).toFloat()
        val maxY = (height - 1).coerceAtLeast(1).toFloat()

        val incoming = LinkedHashMap<Int, SynthFinger>(fingers.size)
        for (f in fingers) {
            if (!incoming.containsKey(f.id) && incoming.size < 10) {
                val px = (f.x.coerceIn(0, 65535) / 65535.0f) * maxX
                val py = (f.y.coerceIn(0, 65535) / 65535.0f) * maxY
                val pressure = (f.pressure.coerceIn(1, 255) / 255.0f)
                incoming[f.id] = SynthFinger(f.id, px, py, pressure)
            }
        }

        val steps = ArrayList<SynthMotionStep>(4)

        // 1. Dita alzate: emette `POINTER_UP` o `UP` prima di rimuoverle dalla lista attiva.
        var i = 0
        while (i < active.size) {
            val existing = active[i]
            if (!incoming.containsKey(existing.id)) {
                val action = if (active.size == 1) SynthAction.UP else SynthAction.POINTER_UP
                steps.add(SynthMotionStep(action, i, active.toList()))
                active.removeAt(i)
            } else {
                i++
            }
        }

        // 2. Dita rimaste appoggiate: aggiorna coordinate ed emette `MOVE` se qualcosa è cambiato.
        var anyMoved = false
        for (idx in active.indices) {
            val prev = active[idx]
            val next = incoming[prev.id] ?: continue
            if (prev.x != next.x || prev.y != next.y || prev.pressure != next.pressure) {
                active[idx] = next
                anyMoved = true
            }
        }
        if (anyMoved && active.isNotEmpty()) {
            steps.add(SynthMotionStep(SynthAction.MOVE, 0, active.toList()))
        }

        // 3. Nuove dita: le aggiunge in coda e poi emette `DOWN` / `POINTER_DOWN`.
        for (f in incoming.values) {
            if (active.none { it.id == f.id }) {
                active.add(f)
                val idx = active.size - 1
                val action = if (active.size == 1) SynthAction.DOWN else SynthAction.POINTER_DOWN
                steps.add(SynthMotionStep(action, idx, active.toList()))
            }
        }

        return steps
    }
}
