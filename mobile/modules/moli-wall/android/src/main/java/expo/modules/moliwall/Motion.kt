package expo.modules.moliwall

import java.nio.ByteBuffer
import kotlin.math.abs

/**
 * Movement in front of the camera, from the brightness of a coarse grid
 * (32 × 24 cells): a cell changes when its brightness moves more than the
 * picture as a whole did. A light switched on, the sun behind a cloud,
 * shift every cell alike and wake nothing; someone passing changes a part of
 * the picture.
 */
class Motion(private val cols: Int = 32, private val rows: Int = 24) {
  private var previous: IntArray? = null

  /** The grid of one frame: the mean brightness of each cell, from the Y
   *  plane (luma) of a camera frame. */
  fun grid(y: ByteBuffer, width: Int, height: Int, rowStride: Int, pixelStride: Int): IntArray {
    val sums = IntArray(cols * rows)
    val counts = IntArray(cols * rows)
    // Every 4th pixel of every 4th row is plenty for a coarse grid.
    val step = 4
    var row = 0
    while (row < height) {
      val cy = row * rows / height
      var col = 0
      while (col < width) {
        val i = row * rowStride + col * pixelStride
        if (i < y.limit()) {
          val cell = cy * cols + col * cols / width
          sums[cell] += y.get(i).toInt() and 0xff
          counts[cell]++
        }
        col += step
      }
      row += step
    }
    return IntArray(cols * rows) { if (counts[it] > 0) sums[it] / counts[it] else 0 }
  }

  /** Whether this frame moved, against the last one. `sensitivity` 1 (a
   *  person close by) to 5 (the least movement). */
  fun moved(grid: IntArray, sensitivity: Int): Boolean {
    val before = previous
    previous = grid
    if (before == null || before.size != grid.size) return false
    val shift = (grid.indices.sumOf { grid[it] - before[it] }) / grid.size
    val s = sensitivity.coerceIn(1, 5)
    // A cell has changed past this much brightness (0–255)…
    val threshold = intArrayOf(40, 32, 25, 18, 12)[s - 1]
    // …and this many cells must have, out of 768.
    val needed = intArrayOf(96, 60, 36, 20, 10)[s - 1]
    var changed = 0
    for (i in grid.indices) {
      if (abs(grid[i] - before[i] - shift) > threshold) changed++
    }
    return changed >= needed
  }

  fun reset() {
    previous = null
  }
}
