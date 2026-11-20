package expo.modules.moliwall

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.SystemClock
import android.util.Size
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleService
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Watches the front camera, a few small frames a second, screen on or off:
 * a movement wakes the screen at once (the page may be asleep with it) and
 * is told to the app (`onMotion`), which keeps the screen on while people
 * are there. Nothing is recorded or sent: each frame is reduced to a grid of
 * brightness, compared with the last, and dropped.
 */
class MotionService : LifecycleService() {
  private lateinit var analysis: ExecutorService
  private val motion = Motion()
  private var lastFrame = 0L
  private var lastTold = 0L

  override fun onCreate() {
    super.onCreate()
    analysis = Executors.newSingleThreadExecutor()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    super.onStartCommand(intent, flags, startId)
    val notification = notification()
    try {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        startForeground(ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
      } else {
        startForeground(ID, notification)
      }
    } catch (_: Exception) {
      // Android refused (camera not granted, started from the background):
      // nothing to watch with.
      stopSelf()
      return START_NOT_STICKY
    }
    bind()
    return START_STICKY
  }

  private fun bind() {
    val future = ProcessCameraProvider.getInstance(this)
    future.addListener({
      val provider = try {
        future.get()
      } catch (_: Exception) {
        stopSelf()
        return@addListener
      }
      val selector = when {
        provider.hasCamera(CameraSelector.DEFAULT_FRONT_CAMERA) -> CameraSelector.DEFAULT_FRONT_CAMERA
        provider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA) -> CameraSelector.DEFAULT_BACK_CAMERA
        else -> {
          stopSelf()
          return@addListener
        }
      }
      val small = ResolutionSelector.Builder()
        .setResolutionStrategy(
          ResolutionStrategy(Size(320, 240), ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER),
        )
        .build()
      val use = ImageAnalysis.Builder()
        .setResolutionSelector(small)
        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
        .build()
      use.setAnalyzer(analysis) { image ->
        image.use {
          val now = SystemClock.elapsedRealtime()
          if (now - lastFrame < EVERY_MS) return@use
          lastFrame = now
          val y = it.planes[0]
          val grid = motion.grid(y.buffer, it.width, it.height, y.rowStride, y.pixelStride)
          if (motion.moved(grid, Wall.settings(this).sensitivity)) onMovement(now)
        }
      }
      try {
        provider.unbindAll()
        provider.bindToLifecycle(this, selector, use)
      } catch (_: Exception) {
        stopSelf()
      }
    }, ContextCompat.getMainExecutor(this))
  }

  private fun onMovement(now: Long) {
    Wall.wake(this)
    if (now - lastTold < TELL_EVERY_MS) return
    lastTold = now
    listener?.invoke()
  }

  private fun notification(): Notification {
    val manager = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      manager?.createNotificationChannel(
        NotificationChannel(CHANNEL, getString(R.string.moli_wall_channel), NotificationManager.IMPORTANCE_MIN).apply {
          setShowBadge(false)
        },
      )
    }
    val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      Notification.Builder(this, CHANNEL)
    } else {
      @Suppress("DEPRECATION")
      Notification.Builder(this)
    }
    return builder
      .setSmallIcon(applicationInfo.icon)
      .setContentTitle("Moli")
      .setContentText(getString(R.string.moli_wall_watching))
      .setOngoing(true)
      .build()
  }

  override fun onDestroy() {
    analysis.shutdown()
    motion.reset()
    super.onDestroy()
  }

  companion object {
    private const val ID = 4207
    private const val CHANNEL = "moli-wall"
    /** Frames looked at: about four a second. */
    private const val EVERY_MS = 250L
    /** The app is told at most every 2 s (a person walking moves every frame). */
    private const val TELL_EVERY_MS = 2_000L

    /** Who hears of a movement (the module, while the app runs). */
    @Volatile var listener: (() -> Unit)? = null

    fun start(context: Context) {
      if (!Wall.hasCamera(context)) return
      try {
        ContextCompat.startForegroundService(context, Intent(context, MotionService::class.java))
      } catch (_: Exception) {
        // started from the background on Android 12+: it starts with the app
      }
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, MotionService::class.java))
    }
  }
}
