# RPM spec for game-mode (repo: greetd_game_mode). Built in COPR from a
# local SRPM produced by packaging/build-srpm.sh (source tarball from the git
# tag + vendored cargo deps as Source1 — no rust-*-devel packages needed).
#
# Best-effort Fedora support for an Arch-first project: steam lives in
# RPM Fusion and tailscale in Tailscale's own repo, so both are weak deps;
# discord and discover-overlay have no Fedora packaging at all (the session
# degrades gracefully without them).
#
# The test suite runs by default; disable for a one-off build with
# --without check.
%bcond_without check

Name:           game-mode
Version:        0.1.5
Release:        1%{?dist}
Summary:        Console-style greetd game mode with gamepad entry and passkey approval
License:        MIT
URL:            https://github.com/MasonRhodesDev/greetd_game_mode
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  systemd-rpm-macros
# libsystemd (journal) + libudev (gilrs) + openssl (verifier)
BuildRequires:  systemd-devel
BuildRequires:  openssl-devel
Requires:       greetd
Requires:       gamescope
# cage runs the greeter (kiosk compositor + regreet). The Hyprland-based
# greeter is retired: a compositor upgrade must never affect the login path.
Requires:       cage
%{?systemd_requires}
# Third-party repos (RPM Fusion / pkgs.tailscale.com) — see the header note.
Recommends:     steam
Recommends:     tailscale

%description
Turns a greetd machine into a console: pressing the gamepad Guide button at
the login greeter swaps the greetd config and boots straight into Steam Big
Picture (inside gamescope, under a bubblewrap home mask), gated by a phone
passkey approval (WebAuthn verifier + Web Push over tailscale). The package
installs files only; host provisioning is `sudo game-mode setup`.

%prep
# -a1 unpacks the vendor tarball (vendor/ at its root) into the source dir.
%autosetup -p1 -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies

%install
# Root crate bins: game-mode, game-mode-steam-config, game-mode-watchdog,
# game-mode-steam-shortcut; then the verifier workspace member.
%cargo_install
(cd verifier && %cargo_install)

# Session helper scripts (referenced by the greetd session configs)
# uwsm-start-hyprland ships as the /usr/bin/start-hyprland the greeter launcher execs
install -Dpm0755 greetd/scripts/uwsm-start-hyprland %{buildroot}%{_bindir}/start-hyprland
install -Dpm0755 greetd/scripts/game-mode-wrapper.sh %{buildroot}%{_bindir}/game-mode-wrapper
install -Dpm0755 greetd/scripts/game-mode-discord %{buildroot}%{_bindir}/game-mode-discord
install -Dpm0755 greetd/scripts/game-mode-overlay %{buildroot}%{_bindir}/game-mode-overlay
install -Dpm0755 greetd/scripts/steamos-session-select %{buildroot}%{_bindir}/steamos-session-select

install -Dpm0644 dist/game-mode.service %{buildroot}%{_unitdir}/game-mode.service
install -Dpm0644 dist/access-gate-verifier.service %{buildroot}%{_unitdir}/access-gate-verifier.service
install -Dpm0644 dist/game-mode.sysusers %{buildroot}%{_sysusersdir}/game-mode.conf
install -Dpm0644 dist/game-mode.tmpfiles %{buildroot}%{_tmpfilesdir}/game-mode.conf
install -d -m0700 %{buildroot}%{_sharedstatedir}/access-gate

# greetd payload: templates rendered into /etc/greetd by `game-mode setup`
for f in config_default.toml game_mode_login.toml regreet.toml environments bg.png; do
    install -Dpm0644 "greetd/$f" "%{buildroot}%{_datadir}/game-mode/greetd/$f"
done
install -Dpm0644 dist/sudoers-greeter-greetd %{buildroot}%{_datadir}/game-mode/sudoers/greeter-greetd

%if %{with check}
%check
%cargo_test
%endif

# The access-gate user itself comes from the sysusers.d snippet via systemd's
# file triggers (no scriptlet needed on current Fedora).
%post
%systemd_post game-mode.service access-gate-verifier.service

%preun
%systemd_preun game-mode.service access-gate-verifier.service

%postun
%systemd_postun_with_restart access-gate-verifier.service

%files
%license LICENSE LICENSE.dependencies
%doc README.md docs/SUSPEND.md
%{_bindir}/game-mode
%{_bindir}/game-mode-steam-config
%{_bindir}/game-mode-watchdog
%{_bindir}/game-mode-steam-shortcut
%{_bindir}/access-gate-verifier
%{_bindir}/start-hyprland
%{_bindir}/game-mode-wrapper
%{_bindir}/game-mode-discord
%{_bindir}/game-mode-overlay
%{_bindir}/steamos-session-select
%{_unitdir}/game-mode.service
%{_unitdir}/access-gate-verifier.service
%{_sysusersdir}/game-mode.conf
%{_tmpfilesdir}/game-mode.conf
%dir %attr(0700, access-gate, access-gate) %{_sharedstatedir}/access-gate
%{_datadir}/game-mode/

%changelog
* Tue Jul 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.5-1
- Greeter: cage -m last -d — fullscreen on one output instead of a window
  centered across the multi-monitor span (cage cannot mirror outputs)

* Tue Jul 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.4-1
- Retire the Hyprland greeter: cage + regreet only. Drops greeter-launcher.sh,
  hypr.conf/hypr.lua, greeter-hint-sync, and the greeter config verification;
  the login path no longer depends on any compositor upgrade.

* Tue Jul 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.3-1
- Ship the launcher-based greeter chain: greeter-launcher.sh + hypr.lua in
  the greetd payload, uwsm-start-hyprland as /usr/bin/start-hyprland,
  Requires cage (the fallback compositor)

* Tue Jul 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.2-1
- setup: run greeter config verification as the greeter user with a
  throwaway XDG_RUNTIME_DIR (Hyprland refuses root; verify aborts unset)

* Fri Jul 03 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.1-1
- Fix first-run CI gates (see git log)

* Thu Jul 02 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.1.0-1
- Initial packaged release: runtime config in /etc/game-mode/config.toml,
  `game-mode setup` provisioning subcommand replaces install.sh
