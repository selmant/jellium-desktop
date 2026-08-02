// Compatibility adapter for the private Jellyfin Web layer. Bootstrap data
// is installed by the native host and never reaches the hosted Foreseer page.
(function installPrivateSessionAdapter() {
    function apply() {
        const bootstrap = window.__jelliumSessionBootstrap;
        const client = window.ApiClient;
        if (!bootstrap || !client) return;

        try {
            if (typeof client.serverAddress === 'function') client.serverAddress(bootstrap.serverUrl);
            if (typeof client.serverId === 'function') client.serverId(bootstrap.serverId);
            if (typeof client.userId === 'function') client.userId(bootstrap.userId);
            if (typeof client.deviceId === 'function') client.deviceId(bootstrap.deviceId);
            if (typeof client.accessToken === 'function') client.accessToken(bootstrap.accessToken);
            if (typeof client.getCurrentUserId === 'function' && client.getCurrentUserId() !== bootstrap.userId) return;
            if (typeof window.jmpNative.jellyfinSessionReady === 'function') {
                window.jmpNative.jellyfinSessionReady(bootstrap.serverId, bootstrap.userId);
                delete window.__jelliumSessionBootstrap;
            }
        } catch (_) {
            // Keep retrying while Jellyfin Web finishes constructing ApiClient.
        }
    }

    window.setInterval(apply, 250);
    apply();
})();
