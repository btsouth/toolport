cask "toolport" do
  version "2.3.8"

  on_arm do
    sha256 "1cd08b43809748dc1c2b9ecbdb68216693e4971e5fc91277850d54fec4c991bd"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg"
  end
  on_intel do
    sha256 "90336f44fd4d7c60f4df483066440e725ad59ca6901f1907dbc7fc1a1a2c7d29"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_x86_64-apple-darwin.dmg"
  end

  name "Toolport"
  desc "One local gateway for every MCP server, shared by every AI client"
  homepage "https://toolport.app/"

  # This file is the only source of the tap's cask. The tap's bump workflow
  # copies it from the release tag and fills in version and sha256 from the
  # published DMGs (see docs/RELEASING.md in the app repo).
  livecheck do
    url :url
    strategy :github_latest
  end

  app "Toolport.app"

  # Homebrew also runs this block when it upgrades or reinstalls the cask, and
  # does not tell the block which. Two independent signals do:
  #  1. The call stack. Upgrades come through Cask::Upgrade.upgrade_cask, which
  #     calls Installer#start_upgrade (Library/Homebrew/cask/upgrade.rb), and
  #     reinstalls through Installer#uninstall_existing_cask
  #     (Library/Homebrew/cask/installer.rb, from cask/reinstall.rb).
  #  2. The brew command this Ruby process runs. brew.sh normalizes it (rm and
  #     remove become uninstall) and passes it as brew.rb's first argument
  #     (Library/Homebrew/brew.sh), so the process's own argv shows it even
  #     though brew.rb then drops it from ARGV. brew bundle runs upgrade,
  #     reinstall and uninstall as child brew processes, so they show here too.
  # Checked against Homebrew/brew 1cd7b3e1341b257955f5d31106637d7696019129.
  # Clients are disconnected only when both signals say this is a real
  # uninstall. A Homebrew change that breaks either signal leaves every client
  # connected and prints how to finish by hand, rather than disconnecting
  # clients during an upgrade.
  #
  # A real uninstall restores each client's config while the gateway still
  # exists, removes Toolport's own launch-at-login entry (it would point at the
  # removed app), and stops the Toolport daemon unless a client is connected
  # right now; that daemon exits on its own when its last client disconnects,
  # because its binary is gone. Zap does the same through Toolport's "Remove
  # Toolport data", which also deletes the registry, logs, backups and this
  # install's keychain items.
  uninstall_preflight do
    frames = caller_locations
    labels = frames.map { |frame| frame.label.to_s.split(/[#.]/).last }
    stack_replacing = frames.any? { |frame| frame.path.to_s.end_with?("/cask/upgrade.rb", "/cask/reinstall.rb") } ||
                      labels.intersect?(%w[start_upgrade revert_upgrade uninstall_existing_cask])
    argv = begin
      command.run("/bin/ps", args: ["-o", "command=", "-p", Process.pid.to_s], print_stderr: false).stdout
    rescue
      ""
    end
    brew_command = argv[%r{/Homebrew/brew\.rb\s+(\S+)}, 1]
    # Test only (scripts/test-homebrew-cask-lifecycle.sh): hide one signal to
    # check that a disagreement disconnects nothing. Hiding a signal can only
    # stop a disconnect, never cause one.
    stack_replacing = false if ENV["HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_STACK"] == "1"
    brew_command = nil if ENV["HOMEBREW_TOOLPORT_CASK_TEST_IGNORE_COMMAND"] == "1"
    command_replacing = %w[upgrade reinstall install].include?(brew_command)
    command_uninstall = %w[uninstall rm remove].include?(brew_command)
    zapping = labels.include?("zap")
    gateway = "#{appdir}/Toolport.app/Contents/MacOS/toolport-gateway"

    if stack_replacing && command_replacing
      ohai "Keeping Toolport's client connections and data for the new version"
    elsif stack_replacing || !command_uninstall
      opoo <<~EOS
        Toolport could not tell whether Homebrew is upgrading or removing it
        (call stack: #{stack_replacing ? "upgrade or reinstall" : "uninstall"}, brew command: #{brew_command || "unknown"}),
        so every client stays connected and all its data stays in place.
        After an upgrade or reinstall nothing more is needed. To remove Toolport,
        reinstall it, run
          "#{gateway}" #{zapping ? "--remove-data --confirm" : "--disconnect-all --uninstall"}
        (or choose "#{zapping ? "Remove Toolport data" : "Remove Toolport from all clients"}" in Settings), then uninstall again.
      EOS
    elsif !File.executable?(gateway)
      opoo <<~EOS
        #{gateway} is missing, so client configs were not restored.
        Reinstall Toolport and run it with --disconnect-all, or choose
        "Remove Toolport from all clients" in Settings, then uninstall again.
      EOS
    elsif zapping
      # Data removal needs the app closed; it then restores clients first.
      command.run("/usr/bin/osascript",
                  args:         ["-e", 'if application id "com.tsout.conduit" is running then ' \
                                       'tell application id "com.tsout.conduit" to quit'],
                  print_stderr: false)
      20.times do
        break unless command.run("/usr/bin/pgrep", args: ["-f", "#{appdir}/Toolport.app/Contents/MacOS/conduit"],
                                                   print_stderr: false).success?

        sleep 0.5
      end
      ohai "Removing Toolport data, credentials and client connections"
      report = File.expand_path("~/Toolport-removal-report.json")
      result = command.run(gateway, args: ["--remove-data", "--confirm"], print_stderr: true)
      File.write(report, result.stdout)
      if result.success?
        ohai "Toolport data removed. Report: #{report}"
      else
        opoo <<~EOS
          Toolport data removal did not finish (exit #{result.exit_status}). Everything
          it could not remove, and why, is listed in #{report}. The registry is kept
          so a retry can finish: reinstall Toolport and choose "Remove Toolport data".
        EOS
      end
    else
      ohai "Restoring client configs, removing launch at login and stopping the Toolport gateway"
      result = command.run(gateway, args: ["--disconnect-all", "--uninstall"], print_stderr: true)
      unless result.success?
        opoo <<~EOS
          Some client configs could not be restored (exit #{result.exit_status}).
          Toolport's data, which holds the original configs, is kept. Reinstall
          Toolport and choose "Remove Toolport from all clients" to retry.
        EOS
      end
    end
  end

  uninstall quit: "com.tsout.conduit"

  # Uninstall keeps data and credentials. Zap removes them (above) and then the
  # app storage; data folders go only once Toolport's removal emptied them.
  zap trash: [
        "~/Library/Application Support/com.tsout.conduit",
        "~/Library/Caches/com.tsout.conduit",
        "~/Library/HTTPStorages/com.tsout.conduit",
        "~/Library/Preferences/com.tsout.conduit.plist",
        "~/Library/Saved Application State/com.tsout.conduit.savedState",
        "~/Library/WebKit/com.tsout.conduit",
      ],
      rmdir: [
        "~/Library/Application Support/Conduit",
        "~/Library/Application Support/Toolport",
      ]

  caveats <<~EOS
    brew uninstall restores every client's config and keeps Toolport's data and
    credentials. brew uninstall --zap also removes them, like "Remove Toolport
    data" in Settings. Upgrades change neither.
  EOS
end
