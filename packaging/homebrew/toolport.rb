cask "toolport" do
  version "1.11.0"

  on_arm do
    sha256 "59688fe3e302a88a61b0c093f734b987390dd7c1d16b02083db92fdeaa95c116"
    url "https://github.com/tsouth89/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
        verified: "github.com/tsouth89/toolport/"
  end
  on_intel do
    sha256 "5ce884fdc62860d235bc4eee4acd9bf45bfc0573d73706f1c45b92956f096f61"
    url "https://github.com/tsouth89/toolport/releases/download/v#{version}/Toolport_x86_64-apple-darwin.dmg",
        verified: "github.com/tsouth89/toolport/"
  end

  name "Toolport"
  desc "One local gateway for every MCP server, shared by every AI client"
  homepage "https://toolport.app/"

  # The updater ships new versions in-app; livecheck tracks the GitHub releases so
  # `brew upgrade` also works for anyone who prefers it.
  livecheck do
    url :url
    strategy :github_latest
  end

  app "Toolport.app"

  # The gateway is a nested helper the app manages; no separate binaries to link.
  zap trash: [
    "~/Library/Application Support/Conduit",
    "~/Library/Caches/com.tsout.conduit",
    "~/Library/HTTPStorages/com.tsout.conduit",
    "~/Library/Preferences/com.tsout.conduit.plist",
    "~/Library/Saved Application State/com.tsout.conduit.savedState",
  ]
end
