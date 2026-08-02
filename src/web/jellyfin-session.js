// Compatibility adapter for the private Jellyfin Web layer. Bootstrap data
// is installed by the native host and never reaches the hosted Foreseer page.
(function installPrivateSessionAdapter() {
    let attempts = 0;
    let timer;
    let generation;
    let validationGeneration;

    function acknowledge(bootstrap, client) {
        const currentUserId = typeof client.getCurrentUserId === 'function'
            ? client.getCurrentUserId()
            : client.userId();
        const normalizedAddress = String(client.serverAddress()).replace(/\/$/, '');
        const expectedAddress = bootstrap.serverUrl.replace(/\/$/, '');
        const matches = normalizedAddress === expectedAddress
            && client.serverId() === bootstrap.serverId
            && currentUserId === bootstrap.userId
            && Boolean(client.deviceId())
            && client.accessToken() === bootstrap.accessToken;
        if (!matches || typeof window.jmpNative.jellyfinSessionReady !== 'function') return false;

        window.jmpNative.jellyfinSessionReady(
            bootstrap.serverId,
            bootstrap.userId,
            bootstrap.generation
        );
        delete window.__jelliumSessionBootstrap;
        if (timer) window.clearInterval(timer);
        return true;
    }

    function apply() {
        const bootstrap = window.__jelliumSessionBootstrap;
        const client = window.ApiClient;
        if (!bootstrap || !client) return;
        if (generation !== bootstrap.generation) {
            generation = bootstrap.generation;
            attempts = 0;
        }
        attempts += 1;
        if (attempts >= 120) {
            if (timer) window.clearInterval(timer);
            if (typeof window.jmpNative.jellyfinSessionFailed === 'function') {
                window.jmpNative.jellyfinSessionFailed(bootstrap.generation);
            }
            delete window.__jelliumSessionBootstrap;
            return;
        }

        try {
            const required = ['serverAddress', 'serverId', 'deviceId', 'accessToken'];
            if (!required.every((name) => typeof client[name] === 'function')) return;
            if (!window._jelliumPlaybackManager || typeof window._jelliumPlaybackManager.play !== 'function') return;

            if (
                typeof client.setAuthenticationInfo === 'function'
                && typeof client.getCurrentUser === 'function'
                && typeof client.getCurrentUserId === 'function'
            ) {
                const hasExpectedIdentity = client.getCurrentUserId() === bootstrap.userId
                    && client.accessToken() === bootstrap.accessToken;
                if (!hasExpectedIdentity) {
                    // Jellyfin 10.11's connection manager clears credentials
                    // while resolving a new server. Wait for that flow to
                    // settle on its private login route before installing the
                    // native session through the supported API.
                    if (!location.hash.toLowerCase().includes('/login')) return;
                    client.serverAddress(bootstrap.serverUrl);
                    if (client.serverId() !== bootstrap.serverId) return;
                    client.setAuthenticationInfo(bootstrap.accessToken, bootstrap.userId);
                }
                if (validationGeneration === bootstrap.generation) return;
                validationGeneration = bootstrap.generation;
                void Promise.resolve(client.getCurrentUser())
                    .then((user) => {
                        validationGeneration = undefined;
                        const current = window.__jelliumSessionBootstrap;
                        if (
                            !current
                            || current.generation !== bootstrap.generation
                            || user?.Id !== bootstrap.userId
                        ) return;
                        acknowledge(bootstrap, client);
                    })
                    .catch(() => {
                        validationGeneration = undefined;
                    });
                return;
            }

            // Compatibility path for older Jellyfin Web ApiClient versions.
            if (typeof client.userId !== 'function') return;
            client.serverAddress(bootstrap.serverUrl);
            client.serverId(bootstrap.serverId);
            client.userId(bootstrap.userId);
            client.deviceId(bootstrap.deviceId);
            client.accessToken(bootstrap.accessToken);
            acknowledge(bootstrap, client);
        } catch (_) {
            // Keep retrying while Jellyfin Web finishes constructing ApiClient.
        }
    }

    window._jelliumApplySessionBootstrap = apply;
    window._jelliumClearSession = function clearSession() {
        delete window.__jelliumSessionBootstrap;
        delete window._jelliumPendingPlayItem;
        if (window._jelliumPlaybackManager?.stop) {
            void Promise.resolve(window._jelliumPlaybackManager.stop()).catch(() => {});
        }
        const client = window.ApiClient;
        if (!client) return;
        if (typeof client.clearAuthenticationInfo === 'function') {
            client.clearAuthenticationInfo();
            return;
        }
        if (typeof client.logout === 'function') {
            void Promise.resolve(client.logout()).catch(() => {});
        }
        for (const name of ['accessToken', 'userId', 'serverId', 'deviceId']) {
            if (typeof client[name] === 'function') client[name](null);
        }
    };
    timer = window.setInterval(apply, 250);
    apply();
})();
