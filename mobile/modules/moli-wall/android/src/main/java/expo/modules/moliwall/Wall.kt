package expo.modules.moliwall

import android.app.Activity
import android.app.admin.DevicePolicyManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import android.os.PowerManager
import android.os.UserManager
import android.view.WindowManager

/** The wall tablet's settings, kept on the tablet: they hold across a restart
 *  (the motion watch starts again before the page has loaded). */
data class WallSettings(
  /** The tablet is a wall tablet: Moli is its home screen and stays in front. */
  val enabled: Boolean = false,
  /** The front camera wakes the screen. */
  val camera: Boolean = true,
  /** 1 (only a person close by) to 5 (the least movement). */
  val sensitivity: Int = 3,
  /** Minutes without anyone before the screen sleeps; 0: never. */
  val idle: Int = 2,
)

object Wall {
  private const val PREFS = "moli.wall"
  /** The app's "home screen" entry (plugins/with-wall.js), off on a phone. */
  private const val HOME_ALIAS = ".WallHome"

  fun settings(context: Context): WallSettings {
    val p = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    return WallSettings(
      enabled = p.getBoolean("enabled", false),
      camera = p.getBoolean("camera", true),
      sensitivity = p.getInt("sensitivity", 3).coerceIn(1, 5),
      idle = p.getInt("idle", 2).coerceIn(0, 120),
    )
  }

  fun save(context: Context, s: WallSettings) {
    context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
      .putBoolean("enabled", s.enabled)
      .putBoolean("camera", s.camera)
      .putInt("sensitivity", s.sensitivity.coerceIn(1, 5))
      .putInt("idle", s.idle.coerceIn(0, 120))
      .apply()
  }

  fun admin(context: Context) = ComponentName(context, WallAdminReceiver::class.java)

  private fun dpm(context: Context) = context.getSystemService(DevicePolicyManager::class.java)

  /** Installed by QR code: Moli runs the tablet. */
  fun isOwner(context: Context) = dpm(context)?.isDeviceOwnerApp(context.packageName) == true

  /** Moli may put the screen to sleep (administrator, or owner). */
  fun isAdmin(context: Context) = dpm(context)?.isAdminActive(admin(context)) == true

  fun hasCamera(context: Context) =
    context.checkSelfPermission(android.Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

  /** A tablet: 600 dp wide at least, or a screen of 7 inches or more (some
   *  tablets declare a density that makes them look narrower). */
  fun isTablet(context: Context): Boolean {
    if (context.resources.configuration.smallestScreenWidthDp >= 600) return true
    val m = context.resources.displayMetrics
    if (m.xdpi <= 0f || m.ydpi <= 0f) return false
    val w = m.widthPixels / m.xdpi
    val h = m.heightPixels / m.ydpi
    return kotlin.math.sqrt(w * w + h * h) >= 7f
  }

  private fun homeAlias(context: Context) = ComponentName(context.packageName, context.packageName + HOME_ALIAS)

  fun isHome(context: Context): Boolean =
    context.packageManager.getComponentEnabledSetting(homeAlias(context)) ==
      PackageManager.COMPONENT_ENABLED_STATE_ENABLED

  /** Moli as the home screen (on), or the tablet's own again (off). */
  fun setHome(context: Context, on: Boolean) {
    val alias = homeAlias(context)
    try {
      context.packageManager.setComponentEnabledSetting(
        alias,
        if (on) PackageManager.COMPONENT_ENABLED_STATE_ENABLED else PackageManager.COMPONENT_ENABLED_STATE_DEFAULT,
        PackageManager.DONT_KILL_APP,
      )
    } catch (_: IllegalArgumentException) {
      return // a build without the entry (plugin left out): nothing to switch
    }
    if (!isOwner(context)) return
    val dpm = dpm(context) ?: return
    val admin = admin(context)
    if (on) {
      val filter = IntentFilter(Intent.ACTION_MAIN).apply {
        addCategory(Intent.CATEGORY_HOME)
        addCategory(Intent.CATEGORY_DEFAULT)
      }
      dpm.addPersistentPreferredActivity(admin, filter, alias)
    } else {
      dpm.clearPackagePersistentPreferredActivities(admin, context.packageName)
    }
  }

  /** As the tablet's owner: Moli alone may stay in front, no lock screen, no
   *  status bar, no other app installed behind its back. Off: all lifted. */
  fun applyOwnerPolicies(context: Context, on: Boolean) {
    if (!isOwner(context)) return
    val dpm = dpm(context) ?: return
    val admin = admin(context)
    dpm.setLockTaskPackages(admin, if (on) arrayOf(context.packageName) else emptyArray())
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      // In front alone, but the power menu stays (switching off for a repair).
      dpm.setLockTaskFeatures(
        admin,
        if (on) DevicePolicyManager.LOCK_TASK_FEATURE_GLOBAL_ACTIONS else DevicePolicyManager.LOCK_TASK_FEATURE_NONE,
      )
    }
    dpm.setKeyguardDisabled(admin, on)
    dpm.setStatusBarDisabled(admin, on)
    for (r in listOf(UserManager.DISALLOW_SAFE_BOOT, UserManager.DISALLOW_ADD_USER)) {
      if (on) dpm.addUserRestriction(admin, r) else dpm.clearUserRestriction(admin, r)
    }
  }

  /** Moli in front: as owner, without asking; otherwise Android's screen
   *  pinning (it asks once, and a long press on back and overview leaves). */
  fun pin(activity: Activity, on: Boolean) {
    try {
      if (on) activity.startLockTask() else activity.stopLockTask()
    } catch (_: IllegalStateException) {
      // not pinned, or already: nothing to do
    } catch (_: SecurityException) {
    }
  }

  fun isPinned(context: Context): Boolean {
    val am = context.getSystemService(android.app.ActivityManager::class.java) ?: return false
    return am.lockTaskModeState != android.app.ActivityManager.LOCK_TASK_MODE_NONE
  }

  /** The window of a wall tablet: shown over the lock screen, turning the
   *  screen on when woken, full screen. On the main thread. */
  fun dress(activity: Activity, on: Boolean) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
      activity.setShowWhenLocked(on)
      activity.setTurnScreenOn(on)
    } else {
      @Suppress("DEPRECATION")
      val flags = WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
      if (on) activity.window.addFlags(flags) else activity.window.clearFlags(flags)
    }
    // The screen stays on while Moli shows: Android's own screen timeout
    // (30 s on a fresh tablet) would put it to sleep long before Moli's
    // `idle` minutes. Moli sleeps it itself (`sleep`, lockNow), and a wake
    // turns it on again with this flag still set.
    if (on) activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    else activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    // A tablet turns freely, wall tablet or not; a phone stays upright.
    activity.requestedOrientation =
      if (on || isTablet(activity)) android.content.pm.ActivityInfo.SCREEN_ORIENTATION_FULL_USER
      else android.content.pm.ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
    // Full screen: the bars come back for a moment with a swipe from the edge.
    val window = activity.window
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val bars = window.insetsController ?: return
      if (on) {
        bars.hide(android.view.WindowInsets.Type.systemBars())
        bars.systemBarsBehavior = android.view.WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
      } else {
        bars.show(android.view.WindowInsets.Type.systemBars())
      }
    } else {
      @Suppress("DEPRECATION")
      window.decorView.systemUiVisibility = if (on) {
        android.view.View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or android.view.View.SYSTEM_UI_FLAG_FULLSCREEN or
          android.view.View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or android.view.View.SYSTEM_UI_FLAG_LAYOUT_STABLE
      } else {
        android.view.View.SYSTEM_UI_FLAG_VISIBLE
      }
    }
  }

  /** The screen on now, if it sleeps. */
  fun wake(context: Context) {
    val pm = context.getSystemService(PowerManager::class.java) ?: return
    if (pm.isInteractive) return
    // The only way left to an app to switch the screen on by itself; still
    // honoured by Android 14.
    @Suppress("DEPRECATION")
    val lock = pm.newWakeLock(
      PowerManager.SCREEN_BRIGHT_WAKE_LOCK or PowerManager.ACQUIRE_CAUSES_WAKEUP or PowerManager.ON_AFTER_RELEASE,
      "moli:wall-wake",
    )
    lock.acquire(3_000)
  }

  /** The screen off now. False when Moli may not (not administrator). */
  fun sleep(context: Context): Boolean {
    if (!isAdmin(context)) return false
    dpm(context)?.lockNow()
    return true
  }

  fun isAwake(context: Context) = context.getSystemService(PowerManager::class.java)?.isInteractive != false
}
