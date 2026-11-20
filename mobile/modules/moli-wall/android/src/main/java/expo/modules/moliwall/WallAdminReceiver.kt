package expo.modules.moliwall

import android.app.Activity
import android.app.admin.DeviceAdminReceiver
import android.app.admin.DevicePolicyManager
import android.content.Context
import android.content.Intent
import android.os.Bundle

/** Moli as the tablet's administrator (screen sleep), and its owner when the
 *  tablet was set up with Moli's QR code. */
class WallAdminReceiver : DeviceAdminReceiver() {
  /** Set up by QR code: the tablet is a wall tablet from the first start. */
  override fun onProfileProvisioningComplete(context: Context, intent: Intent) {
    Wall.save(context, Wall.settings(context).copy(enabled = true))
    Wall.applyOwnerPolicies(context, true)
    Wall.setHome(context, true)
    context.packageManager.getLaunchIntentForPackage(context.packageName)?.let {
      it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      context.startActivity(it)
    }
  }
}

/** The setup wizard asks how the tablet is to be run: fully by Moli. */
class ProvisioningModeActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    // Only asked from Android 10 on (the constants are Android 10's).
    if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.Q) {
      val result = Intent().putExtra(
        DevicePolicyManager.EXTRA_PROVISIONING_MODE,
        DevicePolicyManager.PROVISIONING_MODE_FULLY_MANAGED_DEVICE,
      )
      setResult(RESULT_OK, result)
    }
    finish()
  }
}

/** The setup wizard's last step: nothing more to ask, the policies are set
 *  when provisioning completes. */
class PolicyComplianceActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setResult(RESULT_OK)
    finish()
  }
}
