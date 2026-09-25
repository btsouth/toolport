cask "toolport" do
  version "1.21.1"

  on_arm do
    sha256 "00154cc200cfcbaa9e43bf0a5fb89e0ca531bdb1fe7f1b3835eb72997805665d"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
        verified: "github.com/btsouth/toolport/"
  end
  on_intel do
    sha256 "7bfbc9f09a4c5e7e6fb005b40ac88d928c3dcf32d82db6606273816b061bb7f0"
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
