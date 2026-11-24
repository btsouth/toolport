#!/usr/bin/env node
// @toolport/cli launcher: runs the native toolport binary for this platform.
import { run } from "../lib/launcher.js";

process.exitCode = run();
