package expo.modules.moliapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.util.Log

/** How an update's installation went. On a phone, Android wants the person
 *  to confirm: its screen is shown. */
class InstallStatusReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
    if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
      val confirm =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
          intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
        } else {
          @Suppress("DEPRECATION")
          intent.getParcelableExtra(Intent.EXTRA_INTENT)
        }
      confirm?.let {
        it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(it)
      }
      return
    }
    if (status != PackageInstaller.STATUS_SUCCESS) {
      Log.w("MoliApp", "update not installed: $status ${intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)}")
    }
  }
}

/** Moli was just updated. A tablet Moli runs gets Moli back on screen by
 *  itself; a phone's person opens it when they like. */
class UpdatedReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action != Intent.ACTION_MY_PACKAGE_REPLACED || !isOwner(context)) return
    context.packageManager.getLaunchIntentForPackage(context.packageName)?.let {
      it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      context.startActivity(it)
    }
  }
}
