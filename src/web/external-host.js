// Versioned, deliberately narrow bridge for an allowlisted external frontend.
// The browser process verifies the main-frame origin before acting.
Object.defineProperty(window, 'jelliumHost', {
    configurable: false,
    enumerable: true,
  writable: false,
  value: Object.freeze({
        protocolVersion: 1,
        hostName: 'jellium-desktop',
        hostVersion: '0.1.0',
        capabilities: Object.freeze(['play-item', 'auth-bootstrap', 'player-events', 'session-reset', 'window-controls', 'quit']),
        requestAuthChallenge(requestId) {
            if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(requestId)) {
                return false;
            }
            window.jmpNative.requestAuthChallenge(requestId);
            return true;
        },
        completeAuth(requestId, ticket) {
            if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(requestId)) {
                return false;
            }
            if (typeof ticket !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(ticket)) {
                return false;
            }
            window.jmpNative.completeAuth(requestId, ticket);
            return true;
        },
        clearSession(requestId) {
            if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(requestId)) {
                return false;
            }
            window.jmpNative.clearJellyfinSession(requestId);
            return true;
        },
        playItem(requestId, itemId) {
            if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(requestId)) {
                return false;
            }
            if (typeof itemId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(itemId)) {
                return false;
            }
            window.jmpNative.playJellyfinItem(requestId, itemId);
            return true;
        },
        minimize() {
            window.jmpNative.windowMinimize();
            return true;
        },
        toggleMaximize() {
            window.jmpNative.windowToggleMaximize();
            return true;
        },
        toggleFullscreen() {
            window.jmpNative.toggleFullscreen();
            return true;
        },
        quit() {
            window.jmpNative.appExit();
            return true;
        }
    })
});
