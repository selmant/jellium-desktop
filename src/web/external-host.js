// Versioned, deliberately narrow bridge for an allowlisted external frontend.
// The browser process verifies the main-frame origin before acting.
Object.defineProperty(window, 'jelliumHost', {
    configurable: false,
    enumerable: true,
  writable: false,
  value: Object.freeze({
        protocolVersion: 1,
        hostName: 'foreseer-desktop',
        hostVersion: '0.1.0',
        capabilities: Object.freeze(['play-item', 'window-controls', 'quit']),
        requestAuthChallenge() {
            return false;
        },
        completeAuth() {
            return false;
        },
        playItem(requestId, itemId) {
            if (typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(requestId)) {
                return false;
            }
            if (typeof itemId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(itemId)) {
                return false;
            }
            const admitted = window.jmpNative.playJellyfinItem(requestId, itemId);
            if (admitted === false) return false;
            window.dispatchEvent(new CustomEvent('foreseer:native-event', {
                detail: { protocolVersion: 1, requestId, type: 'accepted' }
            }));
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
