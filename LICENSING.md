# Licensing and binary distribution

DoplarrChaptarr as a combined project is distributed under GPL-3.0-only. A copy
of that license is included in `LICENSE-GPL-3.0`.

The reason for the project-level GPL license is explicit in the source tree:
the generated `sonarr_api` and `radarr_api` package manifests declare GPL-3.0,
and those crates are linked into the `doplarr` executable rather than shipped
as unrelated programs. Binary and container distributors must therefore meet
GPLv3 requirements for the combined work, including providing complete
corresponding source and preserving notices.

Code inherited from Rust Doplarr was offered under MIT or Apache-2.0. Those
permissive licenses are compatible with GPLv3 distribution, and the original
`LICENSE-MIT` and `LICENSE-APACHE` files remain in the repository. This
project-level choice does not erase upstream copyright or license notices.
