package dev.drengr.ime

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.inputmethodservice.InputMethodService
import android.util.Base64

/** Commits text sent by `am broadcast -a dev.drengr.ime.COMMIT --es b64 <base64 UTF-8>`. */
class DrengrIme : InputMethodService() {
    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val b64 = intent.getStringExtra("b64")
            val connection = currentInputConnection
            // With no editor, the connection is a stub that still reports success.
            val committed = b64 != null && connection != null && currentInputStarted &&
                connection.commitText(String(Base64.decode(b64, Base64.DEFAULT), Charsets.UTF_8), 1)
            resultCode = if (committed) 1 else 0
        }
    }

    override fun onCreate() {
        super.onCreate()
        // Only senders holding INJECT_EVENTS (the adb shell) can type through it.
        registerReceiver(receiver, IntentFilter(ACTION), "android.permission.INJECT_EVENTS", null, RECEIVER_EXPORTED)
    }

    override fun onDestroy() {
        unregisterReceiver(receiver)
        super.onDestroy()
    }

    private companion object {
        const val ACTION = "dev.drengr.ime.COMMIT"
    }
}
