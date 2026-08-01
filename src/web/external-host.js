// Deliberately minimal bridge for an allowlisted external frontend.
// The browser process verifies the main-frame origin before acting.
Object.defineProperty(window, 'jelliumHost', {
    configurable: false,
    enumerable: true,
    writable: false,
    value: Object.freeze({
        playItem(itemId) {
            if (typeof itemId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(itemId)) {
                return false;
            }
            window.jmpNative.playJellyfinItem(itemId);
            return true;
        }
    })
});
