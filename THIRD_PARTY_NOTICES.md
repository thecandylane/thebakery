# Third-party notices

The Bakery is MIT-licensed (see LICENSE). These parts come from other projects and keep their own licenses.

| What | Where | License |
|------|-------|---------|
| AI Elements components (Vercel) | `bakery-client/src/lib/ai-elements/` | Apache-2.0, text in `LICENSE` there |
| shadcn/ui components | `bakery-client/src/components/ui/` | MIT, text in `LICENSE.md` there |
| JetBrains Mono font | `bakery-client/static/fonts/` | SIL Open Font License 1.1, text in `OFL.txt` there |
| Patch to `@agentclientprotocol/claude-agent-acp` (Zed Industries) | `bakery-client/patches/` | Apache-2.0, see `README.md` there |
| xAI device sign-in, compaction trigger math, from oh-my-pi | `crates/bakery-agent/src/xai_oauth.rs`, `crates/bakery-node/src/agent_loop.rs` | MIT, below |
| Tool-call stall detection, from hermes-agent | `crates/bakery-node/src/agent_loop.rs` | MIT, below |

## oh-my-pi

https://github.com/can1357/oh-my-pi

```
MIT License

Copyright (c) 2025 Mario Zechner
Copyright (c) 2025-2026 Can Bölük
Copyright (c) 2026 Stencil Labs, Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## hermes-agent

https://github.com/NousResearch/hermes-agent

```
MIT License

Copyright (c) 2025 Nous Research

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
