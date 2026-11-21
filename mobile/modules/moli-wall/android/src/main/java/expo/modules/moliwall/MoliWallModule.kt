package expo.modules.moliwall

import android.app.admin.DevicePolicyManager
import android.content.Intent
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import expo.modules.kotlin.records.Field
import expo.modules.kotlin.records.Record

/** What the page may change (the « Tablette murale » card). */
class WallOptions : Record {
  @Field val enabled: Boolean = false
  @Field val camera: Boolean = true
  @Field val sensitivity: Int = 3
  @Field val idle: Int = 2
}

/**
 * The wall tablet, for the app's shell (src/wall.ts): settings, the screen
 * on and off, Moli in front, and movements seen by the front camera.
 */
class MoliWallModule : Module() {
  private val context get() = requireNotNull(appContext.reactContext) { "no context" }

  override fun definition() = ModuleDefinition {
    Name("MoliWall")
    Events("onMotion")

    OnCreate {
      MotionService.listener = { sendEvent("onMotion", mapOf("at" to System.currentTimeMillis())) }
      // After a restart, before the page has loaded: watching again.
      val s = Wall.settings(context)
      if (s.enabled && s.camera) MotionService.start(context)
    }

    OnDestroy {
      MotionService.listener = null
    }

    OnActivityEntersForeground {
      val activity = appContext.currentActivity ?: return@OnActivityEntersForeground
      val s = Wall.settings(context)
      // An owner keeps Moli in front itself (again after an upkeep
      // `release`); a mere app cannot without asking.
      if (s.enabled && Wall.isOwner(context)) {
        Wall.applyOwnerPolicies(context, true)
        Wall.setHome(context, true)
      }
      activity.runOnUiThread {
        Wall.dress(activity, s.enabled)
        if (s.enabled && Wall.isOwner(context)) Wall.pin(activity, true)
      }
      if (s.enabled && s.camera) MotionService.start(context)
    }

    Function("state") { state() }

    /** The window dressed again (orientation, full screen): the shell calls
     *  it once mounted, should the activity have come before the module. */
    Function("dress") {
      val activity = appContext.currentActivity ?: return@Function false
      val on = Wall.settings(context).enabled
      activity.runOnUiThread { Wall.dress(activity, on) }
      true
    }

    Function("configure") { options: WallOptions ->
      val s = WallSettings(options.enabled, options.camera, options.sensitivity, options.idle)
      Wall.save(context, s)
      Wall.applyOwnerPolicies(context, s.enabled)
      Wall.setHome(context, s.enabled)
      appContext.currentActivity?.let {
        it.runOnUiThread {
          Wall.dress(it, s.enabled)
          if (Wall.isOwner(context)) Wall.pin(it, s.enabled)
        }
      }
      if (s.enabled && s.camera) MotionService.start(context) else MotionService.stop(context)
      state()
    }

    /** Android's screen pinning (not owner): asked once, left with back + overview. */
    Function("pin") { on: Boolean ->
      appContext.currentActivity?.let { it.runOnUiThread { Wall.pin(it, on) } }
      on
    }

    Function("wake") { Wall.wake(context) }

    /** False when Moli may not (not administrator): the shell darkens the page instead. */
    Function("sleep") { Wall.sleep(context) }

    Function("isAwake") { Wall.isAwake(context) }

    /** Android's own screen to make Moli administrator (screen sleep). */
    Function("askAdmin") {
      val intent = Intent(DevicePolicyManager.ACTION_ADD_DEVICE_ADMIN)
        .putExtra(DevicePolicyManager.EXTRA_DEVICE_ADMIN, Wall.admin(context))
        .putExtra(DevicePolicyManager.EXTRA_ADD_EXPLANATION, context.getString(R.string.moli_wall_admin_why))
      (appContext.currentActivity ?: return@Function false).startActivity(intent)
      true
    }

    /** For upkeep: Moli leaves the front until the next start (Settings,
     *  another app). As owner, the tablet's home screen is given back too. */
    Function("release") {
      appContext.currentActivity?.let { it.runOnUiThread { Wall.pin(it, false) } }
      Wall.applyOwnerPolicies(context, false)
      Wall.setHome(context, false)
      state()
    }
  }

  private fun state(): Map<String, Any> {
    val s = Wall.settings(context)
    return mapOf(
      "enabled" to s.enabled,
      "camera" to s.camera,
      "sensitivity" to s.sensitivity,
      "idle" to s.idle,
      "owner" to Wall.isOwner(context),
      "admin" to Wall.isAdmin(context),
      "home" to Wall.isHome(context),
      "pinned" to Wall.isPinned(context),
      "cameraAllowed" to Wall.hasCamera(context),
      "tablet" to Wall.isTablet(context),
    )
  }
}
