package demo

import kotlinx.coroutines.delay

val LOG: java.util.logging.Logger = java.util.logging.Logger.getLogger("orders")

/** Exercised to see what an inline function's body does to the caller's line table. */
inline fun <T> timed(label: String, block: () -> T): T {
    val start = System.nanoTime()
    val result = block()
    LOG.fine("$label took ${System.nanoTime() - start}ns")
    return result
}

data class Order(val id: String, val amount: Double, val kind: Kind)

enum class Kind { RETAIL, WHOLESALE, INTERNAL }

class Processor {
    private var runningTotal: Double = 0.0

    /** Plain function: the control case, should behave like Java. */
    fun plain(prices: List<Double>, rate: Double, apply: Boolean): Double {
        LOG.info("processing ${prices.size} prices")
        val unused = "goes nowhere"
        var inspected = 0
        var subtotal = 0.0
        for (p in prices) {
            if (p < 0) continue
            subtotal += p
            inspected++
        }
        var total = subtotal
        if (apply) {
            total = subtotal * (1.0 + rate)
        }
        LOG.fine("inspected $inspected")
        runningTotal = total
        return total
    }

    /** `when` over an enum: tableswitch or if-chain? */
    fun rateFor(kind: Kind): Double = when (kind) {
        Kind.RETAIL -> 0.20
        Kind.WHOLESALE -> 0.10
        Kind.INTERNAL -> 0.0
    }

    /** Default arguments: generates a `$default` synthetic bridge. */
    fun withDefaults(base: Double, markup: Double = 1.5, label: String = "x"): Double {
        LOG.fine(label)
        return base * markup
    }

    /** Inline call site: where do the inlined body's lines point? */
    fun usesInline(prices: List<Double>): Double = timed("sum") {
        var acc = 0.0
        for (p in prices) {
            acc += p
        }
        acc
    }

    /** Suspend function: compiles to a continuation state machine. */
    suspend fun fetchAndTotal(orders: List<Order>, apply: Boolean): Double {
        LOG.info("fetching ${orders.size}")
        var total = 0.0
        for (o in orders) {
            delay(1)
            val rate = rateFor(o.kind)
            total += if (apply) o.amount * (1.0 + rate) else o.amount
        }
        delay(1)
        runningTotal = total
        return total
    }

    /** Lambda / SAM: generated method's file and lines. */
    fun mapped(prices: List<Double>): List<Double> = prices.map { it * 2.0 }
}
