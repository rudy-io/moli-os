package expo.modules.moliapp

import android.app.PendingIntent
import android.app.admin.DevicePolicyManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Base64
import android.webkit.CookieManager
import expo.modules.kotlin.Promise
import expo.modules.kotlin.exception.CodedException
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.security.SecureRandom
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/**
 * The app itself, for its shell (src/update.ts, src/handoff.ts): its version,
 * its updates downloaded from the house and installed, and the Cloudflare
 * Access session brought back from a Google sign-in made in a browser tab.
 */
class MoliAppModule : Module() {
  private val context get() = requireNotNull(appContext.reactContext) { "no context" }

  override fun definition() = ModuleDefinition {
    Name("MoliApp")

    /** This build's number (Android's versionCode). */
    Function("versionCode") {
      val info = context.packageManager.getPackageInfo(context.packageName, 0)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) info.longVersionCode.toDouble()
      else @Suppress("DEPRECATION") info.versionCode.toDouble()
    }

    /** Moli runs this tablet (device owner): it installs without asking. */
    Function("isOwner") { isOwner(context) }

    /** Android lets Moli install apps (the « unknown sources » permission). */
    Function("canInstall") {
      isOwner(context) || Build.VERSION.SDK_INT < Build.VERSION_CODES.O ||
        context.packageManager.canRequestPackageInstalls()
    }

    /** Android's screen to let Moli install apps. */
    Function("openInstallSettings") {
      val activity = appContext.currentActivity ?: return@Function false
      val intent = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
      activity.startActivity(intent)
      true
    }

    /** A secret and its challenge (RFC 7636, S256), for a sign-in handoff. */
    Function("handoffKeys") {
      val raw = ByteArray(32).also { SecureRandom().nextBytes(it) }
      val verifier = b64url(raw)
      val challenge = b64url(MessageDigest.getInstance("SHA-256").digest(verifier.toByteArray(Charsets.US_ASCII)))
      mapOf("verifier" to verifier, "challenge" to challenge)
    }

    /** The Access session in the app's web view (the cookie Access sets on
     *  the house's own address), kept on disk. */
    AsyncFunction("setSessionCookie") { url: String, token: String, expires: Double?, promise: Promise ->
      val until = expires?.let {
        val f = SimpleDateFormat("EEE, dd MMM yyyy HH:mm:ss 'GMT'", Locale.US)
        f.timeZone = TimeZone.getTimeZone("GMT")
        "; Expires=" + f.format(Date((it * 1000).toLong()))
      } ?: ""
      Handler(Looper.getMainLooper()).post {
        val cookies = CookieManager.getInstance()
        cookies.setAcceptCookie(true)
        cookies.setCookie(url, "CF_Authorization=$token; Path=/; Secure; HttpOnly; SameSite=Lax$until") { ok ->
          cookies.flush()
          promise.resolve(ok)
        }
      }
    }

    /** Downloads the update from the house, checked against its SHA-256:
     *  the file's path. */
    AsyncFunction("download") { url: String, token: String, sha256: String ->
      val dir = File(context.cacheDir, "update").apply { mkdirs() }
      val file = File(dir, "moli.apk")
      file.delete()
      val connection = URL(url).openConnection() as HttpURLConnection
      connection.setRequestProperty("Authorization", "Bearer $token")
      connection.connectTimeout = 15_000
      connection.readTimeout = 60_000
      try {
        if (connection.responseCode != 200) throw UpdateException("HTTP ${connection.responseCode}")
        val digest = MessageDigest.getInstance("SHA-256")
        connection.inputStream.use { input ->
          file.outputStream().use { output ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
              val n = input.read(buffer)
              if (n < 0) break
              digest.update(buffer, 0, n)
              output.write(buffer, 0, n)
            }
          }
        }
        val got = digest.digest().joinToString("") { "%02x".format(it) }
        if (!got.equals(sha256, ignoreCase = true)) {
          file.delete()
          throw UpdateException("SHA-256 mismatch")
        }
      } finally {
        connection.disconnect()
      }
      file.absolutePath
    }

    /** Installs a downloaded update: at once when Moli runs the tablet,
     *  else Android asks the person (InstallStatusReceiver). */
    AsyncFunction("install") { path: String ->
      val file = File(path)
      if (!file.exists()) throw UpdateException("no file")
      val installer = context.packageManager.packageInstaller
      val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
      params.setAppPackageName(context.packageName)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
      }
      val id = installer.createSession(params)
      installer.openSession(id).use { session ->
        file.inputStream().use { input ->
          session.openWrite("moli.apk", 0, file.length()).use { output ->
            input.copyTo(output)
            session.fsync(output)
          }
        }
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or
          (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0)
        val status = PendingIntent.getBroadcast(
          context,
          id,
          Intent(context, InstallStatusReceiver::class.java),
          flags,
        )
        session.commit(status.intentSender)
      }
      true
    }
  }

  private fun b64url(bytes: ByteArray) =
    Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
}

internal fun isOwner(context: Context): Boolean =
  context.getSystemService(DevicePolicyManager::class.java)?.isDeviceOwnerApp(context.packageName) == true

internal class UpdateException(message: String) : CodedException(message)
