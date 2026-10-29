// hyprclick — Hyprland plugin that clicks inside a window without focusing it.
//
// Pointer events are sent straight to the target window's wl_surface at a
// surface-local position, then pointer focus is handed back to whatever is
// under the real cursor. Timing runs on a timerfd inside the compositor's own
// event loop, so there is no IPC round trip per click.
//
// Control (hyprctl socket, one request per line):
//   hyprclick ping                     -> "hyprclick ok <version> <path of this .so>"
//   hyprclick start <selector> <x> <y> <button> <interval_ns> <hold_ns> [only_visible]
//   hyprclick click <selector> <x> <y> <button>
//   hyprclick stop
//   hyprclick status
// <selector> is a Hyprland window selector without spaces, e.g. address:0x55d0c0ffee.
// <button> is a linux input code: 272 left, 273 right, 274 middle.

#include <dlfcn.h>
#include <sys/timerfd.h>
#include <unistd.h>

// Everything from the standard library up front, so none of it gets
// compiled under the #define below.
#include <bits/stdc++.h>

// The seat's pointer list and each pointer's entered surface are private;
// we need them to drive the target's wl_pointer without moving seat focus.
// Must be the first Hyprland header so the include guard doesn't skip it.
#define private public
#include <hyprland/src/protocols/core/Seat.hpp>
#undef private

#include <hyprland/src/plugins/PluginAPI.hpp>
#include <hyprland/src/Compositor.hpp>
#include <hyprland/src/desktop/state/FocusState.hpp>
#include <hyprland/src/desktop/state/ViewState.hpp>
#include <hyprland/src/desktop/state/ViewQuery.hpp>
#include <hyprland/src/desktop/view/Window.hpp>
#include <hyprland/src/helpers/time/Time.hpp>
#include <hyprland/src/managers/SeatManager.hpp>
#include <hyprland/src/managers/input/InputManager.hpp>
#include <hyprland/src/protocols/core/Compositor.hpp>
#include <hyprland/src/xwayland/XSurface.hpp>

inline HANDLE PHANDLE = nullptr;

// Bump on any behaviour change: the app reloads a plugin whose version differs.
constexpr int HYPRCLICK_VERSION = 5;

namespace {
    struct SClicker {
        PHLWINDOWREF       window;
        Vector2D           pos;
        uint32_t           button     = 272;
        uint64_t           intervalNs = 100'000'000;
        uint64_t           holdNs     = 0;
        bool               running    = false;
        bool               pressed    = false;
        bool               restorePending = false;
        WP<CWLSurfaceResource> directSurf; // set while we drive the target's wl_pointer directly
        bool               onlyVisible = false; // pause while the target's workspace isn't shown
        bool               paused      = false;
        uint64_t           clicks     = 0;
        std::string        lastError;
        Time::steady_tp    raisedAt;

        int                tickFd  = -1;
        int                holdFd  = -1;
        int                restoreFd  = -1;
        int                fireFd  = -1;
        wl_event_source*   tickSrc = nullptr;
        wl_event_source*   fireSrc = nullptr;
        wl_event_source*   holdSrc = nullptr;
        wl_event_source*   restoreSrc = nullptr;
    };

    SClicker g_clicker;

    void armTimer(int fd, uint64_t firstNs, uint64_t periodNs) {
        itimerspec spec{};
        spec.it_value.tv_sec     = firstNs / 1'000'000'000ULL;
        spec.it_value.tv_nsec    = firstNs % 1'000'000'000ULL;
        spec.it_interval.tv_sec  = periodNs / 1'000'000'000ULL;
        spec.it_interval.tv_nsec = periodNs % 1'000'000'000ULL;
        timerfd_settime(fd, 0, &spec, nullptr);
    }

    void drain(int fd) {
        uint64_t expirations = 0;
        (void)read(fd, &expirations, sizeof(expirations));
    }

    PHLWINDOW resolve(const std::string& selector) {
        return Desktop::viewState()->query().selector(selector).mappedOnly().runWindow();
    }

    // Xwayland applies wl_pointer.leave immediately but only queues button
    // events, so restoring focus in the same batch makes X drop the click.
    // Keep focus on X11 targets this long before handing it back.
    constexpr uint64_t X11_RESTORE_NS = 3'000'000;

    void restoreFocus() {
        g_clicker.restorePending = false;

        if (const auto DIRECT = g_clicker.directSurf.lock()) {
            g_clicker.directSurf.reset();
            // The seat never moved, so just leave the target again, unless the
            // user's real pointer has since moved onto it.
            if (g_pSeatManager->m_state.pointerFocus.lock() == DIRECT)
                return;
            for (const auto& p : PROTO::seat->m_pointers) {
                if (p && p->good() && p->m_currentSurface == DIRECT) {
                    p->sendLeave();
                    p->sendFrame();
                }
            }
            return;
        }

        // Recomputes focus + local coords from the real cursor, same path a
        // physical mouse move takes. Also covers the cursor already being over
        // the target: it moves the client's pointer back to the real spot.
        g_pInputManager->simulateMouseMovement();

        // We may have raised the target over the X window the user is
        // hovering; put that one back on top so their real clicks land there.
        const auto FOCUS = g_pSeatManager->m_state.pointerFocus.lock();
        if (!FOCUS)
            return;
        const auto UNDER = Desktop::viewState()->query().surface(FOCUS).runWindow();
        if (!UNDER || !UNDER->m_isX11 || UNDER == g_clicker.window.lock())
            return;
        if (const auto XSURF = UNDER->m_xwaylandSurface.lock()) {
            XSURF->restackToTop();
            g_clicker.raisedAt = {}; // target is no longer on top
        }
    }

    // The target client's wl_pointers, when we can drive them without touching
    // the seat: none of them is in use on another surface (the user isn't
    // hovering another window of the same client, e.g. another X11 window) and
    // the user's pointer isn't on the target itself. Then the user's own
    // pointer focus never changes (no hover/cursor flicker), and the target's
    // cursor requests are ignored since it doesn't hold seat focus.
    std::vector<SP<CWLPointerResource>> directPointers(SP<CWLSurfaceResource> surf) {
        if (g_pSeatManager->m_state.pointerFocus.lock() == surf)
            return {};
        std::vector<SP<CWLPointerResource>> out;
        for (const auto& p : PROTO::seat->m_pointers) {
            if (!p || !p->good() || !p->m_owner || p->m_owner->client() != surf->client())
                continue;
            if (p->m_currentSurface && p->m_currentSurface != surf)
                return {};
            out.push_back(p);
        }
        return out;
    }

    // Games (Roblox) ignore clicks while unfocused. Mark an X11 target as the
    // active X window; only while the user's focused window is native Wayland,
    // so no X11 app the user types into loses its keyboard. Wayland keyboard
    // focus is untouched either way.
    void activateX11(PHLWINDOW window) {
        if (!window->m_isX11)
            return;
        const auto FOCUSED = Desktop::focusState()->window();
        if (FOCUSED && FOCUSED->m_isX11)
            return;
        if (const auto XSURF = window->m_xwaylandSurface.lock())
            XSURF->activate(true);
    }

    // Send button transitions to `window` at surface-local `pos`, then give
    // pointer focus back to the surface under the user's real cursor.
    // `holding`: the button stays down, so focus stays on the target until the
    // release. Xwayland drops held buttons when the pointer enters another X
    // window, and Wayland toolkits may cancel a press on leave.
    bool sendButtons(PHLWINDOW window, const Vector2D& pos, uint32_t button, std::initializer_list<wl_pointer_button_state> states, bool holding = false) {
        if (!window || !window->m_isMapped || !window->wlSurface())
            return false;

        const auto SURF = window->wlSurface()->resource();
        if (!SURF)
            return false;

        const auto NOW     = (uint32_t)Time::millis(Time::steadyNow());
        const auto POINTERS = directPointers(SURF);

        if (!POINTERS.empty()) {
            for (const auto& p : POINTERS) {
                p->sendEnter(SURF, pos); // no-op if still entered from the last click
                p->sendMotion(NOW, pos);
                p->sendFrame();
                for (const auto STATE : states) {
                    p->sendButton(NOW, button, STATE);
                    p->sendFrame();
                }
            }
            g_clicker.directSurf = SURF;
        } else {
            g_pSeatManager->setPointerFocus(SURF, pos);
            g_pSeatManager->sendPointerMotion(NOW, pos);
            g_pSeatManager->sendPointerFrame();
            for (const auto STATE : states) {
                g_pSeatManager->sendPointerButton(NOW, button, STATE);
                g_pSeatManager->sendPointerFrame();
            }
        }

        if (holding) {
            g_clicker.restorePending = true;
            armTimer(g_clicker.restoreFd, 0, 0);
        } else if (window->m_isX11) {
            g_clicker.restorePending = true;
            armTimer(g_clicker.restoreFd, X11_RESTORE_NS, 0);
        } else
            restoreFocus();
        return true;
    }

    void release() {
        auto& C = g_clicker;
        if (!C.pressed)
            return;
        sendButtons(C.window.lock(), C.pos, C.button, {WL_POINTER_BUTTON_STATE_RELEASED});
        C.pressed = false;
    }

    void stopClicker(const std::string& reason = "") {
        auto& C = g_clicker;
        release();
        if (C.tickFd >= 0)
            armTimer(C.tickFd, 0, 0);
        if (C.holdFd >= 0)
            armTimer(C.holdFd, 0, 0);
        if (C.fireFd >= 0)
            armTimer(C.fireFd, 0, 0);
        C.running   = false;
        C.lastError = reason;
    }

    int onRestore(int fd, uint32_t, void*) {
        drain(fd);
        if (g_clicker.restorePending)
            restoreFocus();
        return 0;
    }

    int onHold(int fd, uint32_t, void*) {
        drain(fd);
        release();
        return 0;
    }

    // Xwayland hit-tests the pointer against X stacking order only, and windows
    // on other workspaces overlap in X coordinates, so a click could land in
    // whichever X window is on top there. Raise the target in X first (purely
    // X-side, nothing changes on screen).
    constexpr uint64_t X11_RAISE_SETTLE_NS = 2'000'000;
    constexpr auto     X11_RAISE_FRESH     = std::chrono::milliseconds(50);

    // Returns true when the click should wait for the raise to be applied.
    bool raiseX11(PHLWINDOW window) {
        if (!window->m_isX11)
            return false;
        const auto XSURF = window->m_xwaylandSurface.lock();
        if (!XSURF)
            return false;
        const auto NOW   = Time::steadyNow();
        const bool FRESH = NOW - g_clicker.raisedAt < X11_RAISE_FRESH;
        XSURF->restackToTop();
        g_clicker.raisedAt = NOW;
        return !FRESH;
    }

    void fire() {
        auto&      C      = g_clicker;
        const auto WINDOW = C.window.lock();
        if (!C.running || !WINDOW || !WINDOW->m_isMapped)
            return;

        activateX11(WINDOW);

        if (C.holdNs == 0) {
            if (!sendButtons(WINDOW, C.pos, C.button, {WL_POINTER_BUTTON_STATE_PRESSED, WL_POINTER_BUTTON_STATE_RELEASED}))
                return;
        } else {
            if (!sendButtons(WINDOW, C.pos, C.button, {WL_POINTER_BUTTON_STATE_PRESSED}, true))
                return;
            C.pressed = true;
            armTimer(C.holdFd, C.holdNs, 0);
        }

        C.clicks++;
    }

    int onFire(int fd, uint32_t, void*) {
        drain(fd);
        fire();
        return 0;
    }

    int onTick(int fd, uint32_t, void*) {
        drain(fd);
        auto& C = g_clicker;
        if (!C.running)
            return 0;

        const auto WINDOW = C.window.lock();
        if (!WINDOW || !WINDOW->m_isMapped) {
            stopClicker("target window closed");
            return 0;
        }

        // A hold longer than the interval would swallow the next press.
        release();

        C.paused = C.onlyVisible && !(WINDOW->m_workspace && WINDOW->m_workspace->isVisible());
        if (C.paused)
            return 0;

        if (raiseX11(WINDOW))
            armTimer(C.fireFd, X11_RAISE_SETTLE_NS, 0);
        else
            fire();
        return 0;
    }

    std::string cmdStart(std::istringstream& args) {
        std::string selector;
        double      x = 0, y = 0;
        uint32_t    button = 272;
        uint64_t    intervalNs = 0, holdNs = 0;
        uint32_t    onlyVisible = 0;
        if (!(args >> selector >> x >> y >> button >> intervalNs >> holdNs))
            return "error: usage: hyprclick start <selector> <x> <y> <button> <interval_ns> <hold_ns> [only_visible]";
        args >> onlyVisible;
        if (button < 272 || button > 279)
            return "error: button must be 272..279";
        if (intervalNs < 100'000)
            return "error: interval below 0.1ms";

        const auto WINDOW = resolve(selector);
        if (!WINDOW)
            return "error: window not found";

        stopClicker();
        auto& C      = g_clicker;
        C.window     = WINDOW;
        C.pos        = {x, y};
        C.button     = button;
        C.intervalNs = intervalNs;
        C.holdNs     = std::min(holdNs, intervalNs * 9 / 10);
        C.clicks      = 0;
        C.onlyVisible = onlyVisible != 0;
        C.paused      = false;
        C.running     = true;
        C.lastError.clear();
        // First click fires right away, then on a fixed, drift-free period.
        armTimer(C.tickFd, 1, intervalNs);
        return "ok";
    }

    std::string cmdClick(std::istringstream& args) {
        std::string selector;
        double      x = 0, y = 0;
        uint32_t    button = 272;
        if (!(args >> selector >> x >> y >> button))
            return "error: usage: hyprclick click <selector> <x> <y> <button>";
        const auto WINDOW = resolve(selector);
        if (!WINDOW)
            return "error: window not found";
        if (raiseX11(WINDOW))
            usleep(X11_RAISE_SETTLE_NS / 1000);
        sendButtons(WINDOW, {x, y}, button, {WL_POINTER_BUTTON_STATE_PRESSED, WL_POINTER_BUTTON_STATE_RELEASED});
        return "ok";
    }

    std::string onCommand(eHyprCtlOutputFormat, std::string request) {
        std::istringstream args(request);
        std::string        name, sub;
        args >> name >> sub;

        if (sub == "ping") {
            Dl_info info{};
            dladdr((void*)&onCommand, &info);
            return std::format("hyprclick ok {} {}", HYPRCLICK_VERSION, info.dli_fname ? info.dli_fname : "?");
        }
        if (sub == "start")
            return cmdStart(args);
        if (sub == "click")
            return cmdClick(args);
        if (sub == "stop") {
            stopClicker();
            return "ok";
        }
        if (sub == "status") {
            const auto& C = g_clicker;
            return std::format("running={} paused={} clicks={} error={}", C.running ? 1 : 0, C.paused ? 1 : 0, C.clicks, C.lastError.empty() ? "-" : C.lastError);
        }
        return "error: unknown subcommand";
    }
}

APICALL EXPORT std::string PLUGIN_API_VERSION() {
    return HYPRLAND_API_VERSION;
}

APICALL EXPORT PLUGIN_DESCRIPTION_INFO PLUGIN_INIT(HANDLE handle) {
    PHANDLE = handle;

    const std::string HASH        = __hyprland_api_get_hash();
    const std::string CLIENT_HASH = __hyprland_api_get_client_hash();
    if (HASH != CLIENT_HASH)
        throw std::runtime_error("[hyprclick] built for a different Hyprland version, rebuild it");

    auto& C    = g_clicker;
    C.tickFd   = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    C.holdFd   = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    C.tickSrc  = wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, C.tickFd, WL_EVENT_READABLE, onTick, nullptr);
    C.holdSrc  = wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, C.holdFd, WL_EVENT_READABLE, onHold, nullptr);
    C.restoreFd  = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    C.restoreSrc = wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, C.restoreFd, WL_EVENT_READABLE, onRestore, nullptr);
    C.fireFd     = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
    C.fireSrc    = wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, C.fireFd, WL_EVENT_READABLE, onFire, nullptr);

    HyprlandAPI::registerHyprCtlCommand(PHANDLE, SHyprCtlCommand{.name = "hyprclick", .exact = false, .fn = onCommand});

    return {"hyprclick", "Click inside windows without focusing them", "ari", "1.0"};
}

APICALL EXPORT void PLUGIN_EXIT() {
    auto& C = g_clicker;
    stopClicker();
    if (C.tickSrc)
        wl_event_source_remove(C.tickSrc);
    if (C.holdSrc)
        wl_event_source_remove(C.holdSrc);
    if (C.restoreSrc)
        wl_event_source_remove(C.restoreSrc);
    if (C.fireSrc)
        wl_event_source_remove(C.fireSrc);
    if (C.tickFd >= 0)
        close(C.tickFd);
    if (C.holdFd >= 0)
        close(C.holdFd);
    if (C.restoreFd >= 0)
        close(C.restoreFd);
    if (C.fireFd >= 0)
        close(C.fireFd);
    if (C.restorePending)
        g_pInputManager->simulateMouseMovement();
    C = SClicker{};
}
