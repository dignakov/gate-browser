Third-party license texts that the packages ship (`cargo xtask package`
copies them into `licenses/` beside Gate) and that aren't elsewhere in the
tree:

- `cef-LICENSE.txt`: the Chromium Embedded Framework (BSD-3-Clause), from
  https://github.com/chromiumembedded/cef/blob/master/LICENSE.txt. CEF's
  binary downloads don't include it. Chromium's own notices
  (`CREDITS.html`) come with the CEF download.
