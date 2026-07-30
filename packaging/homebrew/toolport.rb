cask "toolport" do
  version "1.10.0"

  on_arm do
    sha256 "47df203c2464e6765103b6f4304d426bb5e476392df0e914663a161fe5243e24"
    url "https://github.com/tsouth89/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg",
        verified: "github.com/tsouth89/toolport/"
  end
  on_intel do
    sha256 "33094f1f693a69e75677d926c35fc7b532b0fdd69097b8b728ce4d538282acea"
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
