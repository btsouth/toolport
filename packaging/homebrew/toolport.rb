cask "toolport" do
  version "2.0.0-preview.4"

  on_arm do
    sha256 "67b5db8b2c7d76f03f64de74b2fa9c93217f04cb9ff345d0c6e2f58122ee7160"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
        verified: "github.com/btsouth/toolport/"
  end
  on_intel do
    sha256 "a69c7de67504b30da86c05f6797fc5fa86c31b978b8c218833d881854cb061b7"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_x86_64-apple-darwin.dmg",
        verified: "github.com/btsouth/toolport/"
  end

  name "Toolport"
  desc "One local gateway for every MCP server, shared by every AI client"
  homepage "https://toolport.app/"

  # livecheck reports the latest GitHub tag for `brew livecheck`. brew install
  # and brew upgrade still use the pinned version + sha256 above. Bump those
  # on each published release (see docs/RELEASING.md).
  livecheck do
    url :url
    strategy :github_latest
  end

  app "Toolport.app"

  # Homebrew also runs uninstall_preflight on upgrades without exposing the
  # upgrade flag to that block. Disconnecting there would break live clients.
  caveats <<~EOS
    Before brew uninstall or zap, open Toolport Settings and choose
    "Remove Toolport from all clients". Keep Toolport's data if cleanup fails.
    Or run "#{appdir}/Toolport.app/Contents/MacOS/toolport-gateway" --disconnect-all
    as your normal user while the app is still installed.
  EOS

  # The gateway is a nested helper the app manages; no separate binaries to link.
  # Application Support: current leaf is Toolport (brand.rs data_dir_leaf_name);
  # Conduit remains for installs that have not migrated. Cache/pref paths keep
  # com.tsout.conduit because the bundle id is intentionally unchanged.
  zap trash: [
    "~/Library/Application Support/Conduit",
    "~/Library/Application Support/Toolport",
    "~/Library/Caches/com.tsout.conduit",
    "~/Library/HTTPStorages/com.tsout.conduit",
    "~/Library/Preferences/com.tsout.conduit.plist",
    "~/Library/Saved Application State/com.tsout.conduit.savedState",
  ]
end
