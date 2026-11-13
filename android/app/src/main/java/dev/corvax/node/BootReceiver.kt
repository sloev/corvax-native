package dev.corvax.node

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Start the node after a reboot, if the user turned it on once (and hasn't paused it). */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        if (i.action == Intent.ACTION_BOOT_COMPLETED && c.getSharedPreferences("corvax", Context.MODE_PRIVATE).getBoolean("enabled", false)) NodeService.start(c)
    }
}
