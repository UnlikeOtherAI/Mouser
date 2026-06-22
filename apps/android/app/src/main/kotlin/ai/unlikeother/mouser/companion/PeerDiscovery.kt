package ai.unlikeother.mouser.companion

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.InetAddress
import java.util.concurrent.Executors

/**
 * A `mouserd` peer found on the LAN over mDNS / DNS-SD (§4). The [id] is the
 * peer's base32 `device_id` (TXT key `id`) — what [MouserClient.connect] pins the
 * QUIC dial against; trust is established from this, never from the rest of TXT.
 */
data class DiscoveredPeer(
    val id: String,
    val name: String,
    val host: InetAddress,
    val port: Int,
) {
    /** Stable key for list diffing / pruning, independent of re-resolution. */
    val key: String get() = id

    /** Literal address string for the dial (never null: an unresolvable peer is
     *  dropped before a [DiscoveredPeer] is built). */
    val hostAddress: String get() = host.hostAddress ?: host.hostName
}

/**
 * LAN peer discovery for the companion using the platform [NsdManager]
 * (DNS-SD). It browses for `_mouser._udp` services advertised by `mouserd`
 * (§4), resolves each to a `host:port` + TXT `id` (base32 `device_id`), and
 * publishes the live set as a [StateFlow] the UI observes; tapping a peer drives
 * [MouserClient.connect].
 *
 * Why NsdManager (not raw mDNS): it is the supported Android path, handles the
 * multicast group join, and resolves SRV/A/TXT for us. We still hold a
 * [WifiManager.MulticastLock] while browsing — without it many devices silently
 * drop inbound multicast once the Wi-Fi power-save filter engages, so the
 * `onServiceFound` callbacks never fire.
 *
 * Resolution API split (handled cleanly per level):
 *  - **API ≥ 34**: the legacy `resolveService` is deprecated and single-shot.
 *    We use [NsdManager.registerServiceInfoCallback], which delivers the
 *    resolved info and subsequent updates, and unregister it on loss.
 *  - **API 26–33**: each found service gets its own fresh
 *    [NsdManager.ResolveListener] (the platform forbids reusing one across
 *    concurrent resolves).
 *
 * The published list is keyed by base32 `device_id`, so re-resolves replace
 * rather than duplicate, and [stop] clears everything and releases the lock.
 */
class PeerDiscovery(context: Context) {

    private val appContext = context.applicationContext
    private val nsdManager = appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val wifiManager =
        appContext.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager

    /** Serialises all NSD bookkeeping (peers map, per-service callbacks). */
    private val executor = Executors.newSingleThreadExecutor()

    private val _peers = MutableStateFlow<List<DiscoveredPeer>>(emptyList())
    val peers: StateFlow<List<DiscoveredPeer>> = _peers.asStateFlow()

    private var multicastLock: WifiManager.MulticastLock? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null

    /** Per-service resolve bookkeeping so we can unregister the API-34 callbacks. */
    private val serviceCallbacks = HashMap<String, Any>()
    /** DNS-SD service name → published peer id, so `onServiceLost` can prune. */
    private val serviceToPeerId = HashMap<String, String>()

    /** Start browsing (idempotent). Acquires the multicast lock first. */
    fun start() {
        if (discoveryListener != null) return
        acquireMulticastLock()
        val listener = makeDiscoveryListener()
        discoveryListener = listener
        runCatching {
            nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
        }.onFailure {
            Log.w(TAG, "discoverServices failed", it)
            discoveryListener = null
            releaseMulticastLock()
        }
    }

    /** Stop browsing, drop all resolves, clear the list, release the lock. */
    fun stop() {
        discoveryListener?.let { listener ->
            runCatching { nsdManager.stopServiceDiscovery(listener) }
                .onFailure { Log.w(TAG, "stopServiceDiscovery failed", it) }
        }
        discoveryListener = null
        executor.execute {
            serviceCallbacks.values.forEach(::unregisterResolve)
            serviceCallbacks.clear()
            serviceToPeerId.clear()
            _peers.value = emptyList()
        }
        releaseMulticastLock()
    }

    private fun makeDiscoveryListener() = object : NsdManager.DiscoveryListener {
        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            Log.w(TAG, "onStartDiscoveryFailed: $errorCode")
            releaseMulticastLock()
        }

        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
            Log.w(TAG, "onStopDiscoveryFailed: $errorCode")
        }

        override fun onDiscoveryStarted(serviceType: String) {
            Log.d(TAG, "discovery started for $serviceType")
        }

        override fun onDiscoveryStopped(serviceType: String) {
            Log.d(TAG, "discovery stopped for $serviceType")
        }

        override fun onServiceFound(serviceInfo: NsdServiceInfo) {
            // Filter by type — NSD can surface unrelated services on some OEMs.
            if (!serviceInfo.serviceType.contains(SERVICE_TYPE.trimEnd('.'))) return
            executor.execute { resolve(serviceInfo) }
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) {
            executor.execute { prune(serviceInfo.serviceName) }
        }
    }

    private fun resolve(serviceInfo: NsdServiceInfo) {
        val name = serviceInfo.serviceName
        if (serviceCallbacks.containsKey(name)) return // already resolving / resolved
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            resolveWithInfoCallback(name, serviceInfo)
        } else {
            resolveWithListener(name, serviceInfo)
        }
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    private fun resolveWithInfoCallback(name: String, serviceInfo: NsdServiceInfo) {
        val callback = object : NsdManager.ServiceInfoCallback {
            override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) {
                Log.w(TAG, "registerServiceInfoCallback failed: $errorCode")
                executor.execute { serviceCallbacks.remove(name) }
            }

            override fun onServiceUpdated(info: NsdServiceInfo) {
                executor.execute { publish(name, info) }
            }

            override fun onServiceLost() {
                executor.execute { prune(name) }
            }

            override fun onServiceInfoCallbackUnregistered() {}
        }
        serviceCallbacks[name] = callback
        runCatching {
            nsdManager.registerServiceInfoCallback(serviceInfo, executor, callback)
        }.onFailure {
            Log.w(TAG, "registerServiceInfoCallback threw", it)
            serviceCallbacks.remove(name)
        }
    }

    @Suppress("DEPRECATION")
    private fun resolveWithListener(name: String, serviceInfo: NsdServiceInfo) {
        val listener = object : NsdManager.ResolveListener {
            override fun onResolveFailed(failedInfo: NsdServiceInfo, errorCode: Int) {
                Log.w(TAG, "resolve failed for $name: $errorCode")
                executor.execute { serviceCallbacks.remove(name) }
            }

            override fun onServiceResolved(resolvedInfo: NsdServiceInfo) {
                executor.execute { publish(name, resolvedInfo) }
            }
        }
        serviceCallbacks[name] = listener
        runCatching { nsdManager.resolveService(serviceInfo, listener) }
            .onFailure {
                Log.w(TAG, "resolveService threw", it)
                serviceCallbacks.remove(name)
            }
    }

    /** Translate a resolved [NsdServiceInfo] into a [DiscoveredPeer] and publish it. */
    private fun publish(serviceName: String, info: NsdServiceInfo) {
        val host = info.hostAddress() ?: return
        val port = info.port
        if (port <= 0) return
        val id = info.txtId() ?: return // §4: no `id` → not dialable, skip.
        val name = info.txtName() ?: serviceName
        val peer = DiscoveredPeer(id = id, name = name, host = host, port = port)
        serviceToPeerId[serviceName] = id
        _peers.value = _peers.value.filterNot { it.key == peer.key } + peer
    }

    /** Remove the peer a departed DNS-SD service mapped to, and drop its resolve. */
    private fun prune(serviceName: String) {
        serviceCallbacks.remove(serviceName)?.let(::unregisterResolve)
        val id = serviceToPeerId.remove(serviceName) ?: return
        _peers.value = _peers.value.filterNot { it.key == id }
    }

    private fun unregisterResolve(callback: Any) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE &&
            callback is NsdManager.ServiceInfoCallback
        ) {
            runCatching { nsdManager.unregisterServiceInfoCallback(callback) }
                .onFailure { Log.w(TAG, "unregisterServiceInfoCallback failed", it) }
        }
        // The pre-34 ResolveListener is single-shot; nothing to unregister.
    }

    private fun acquireMulticastLock() {
        if (multicastLock?.isHeld == true) return
        val lock = wifiManager.createMulticastLock(MULTICAST_LOCK_TAG).apply {
            setReferenceCounted(false)
        }
        runCatching { lock.acquire() }
            .onSuccess { multicastLock = lock }
            .onFailure { Log.w(TAG, "multicast lock acquire failed", it) }
    }

    private fun releaseMulticastLock() {
        multicastLock?.let { lock ->
            if (lock.isHeld) runCatching { lock.release() }
                .onFailure { Log.w(TAG, "multicast lock release failed", it) }
        }
        multicastLock = null
    }

    private companion object {
        const val TAG = "PeerDiscovery"
        const val MULTICAST_LOCK_TAG = "mouser.nsd"

        // NSD takes the type WITHOUT the trailing `.local.` the mDNS wire form
        // (mouser-net SERVICE_TYPE) uses; the platform appends the domain itself.
        const val SERVICE_TYPE = "_mouser._udp"
    }
}

/** The first usable resolved address (NSD exposes one or many depending on level). */
private fun NsdServiceInfo.hostAddress(): InetAddress? =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        @Suppress("DEPRECATION")
        hostAddresses.firstOrNull() ?: host
    } else {
        @Suppress("DEPRECATION")
        host
    }

/** Base32 `device_id` from the TXT `id` key (§4); `null` if absent. */
private fun NsdServiceInfo.txtId(): String? = txtString("id")

/** Display `name` from the TXT `name` key (§4); `null` if absent. */
private fun NsdServiceInfo.txtName(): String? = txtString("name")

private fun NsdServiceInfo.txtString(key: String): String? =
    attributes[key]?.let { String(it, Charsets.UTF_8) }?.takeIf { it.isNotEmpty() }
