---
"ikenga-desktop": patch
---

The Chi Companion's target picker is now a well-formed menu. Its options were each wrapped in a bare `<div>`, so `role="menu"` owned no `menuitem*` or `group` children (an `aria-required-children` violation that lets an accessibility tree flatten the options into text); same-group options now sit inside one `role="group"` that carries the caption as its accessible name. The chip also follows the APG menu-button pattern: ArrowDown opens it on the first option, ArrowUp on the last, and Home / End move to either end of the open menu.
