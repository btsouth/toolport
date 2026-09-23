cask "toolport" do
  version "1.20.0"

  on_arm do
    sha256 "7016879ee2f20d8a2669cc325a1366346e8f0846c7db04c303a8bacf804eae53"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
        verified: "github.com/btsouth/toolport/"
  end
  on_intel do
    sha256 "2ba559e06c1375d723956a52b9c4307be5a8078e030a204bc5fddea371d0a935"
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
