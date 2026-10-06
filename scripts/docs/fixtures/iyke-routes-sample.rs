// Synthetic sample in the shape of a router block. It is not the real file.
pub fn build_router() -> Router {
    let authed = Router::new()
        .route("/iyke/state", get(get_state))
        // Example comment.
        // .route("/iyke/commented-out", get(never_used))
        .route("/iyke/items", get(list_items))
        .route("/iyke/items/add", post(add_item))
        .route("/iyke/items/remove", post(remove_item))
        .route("/iyke/things/*thing_id", get(get_thing))
        .route("/iyke/things/preview/:thing_id", get(preview_thing))
        .route("/iyke/things/reset", post(reset_things))
        .route(
            "/iyke/things/archive/all",
            post(archive_all_things),
        )
        .route(
            "/iyke/things/export",
            get(export_things).post(import_things),
        )
        .route("/iyke/misc/ping", axum::routing::get(ping).delete(clear_ping))
        .route("/iyke/misc/put", put(put_misc))
        .route("/iyke/misc/patch", patch(patch_misc))
        .route("/iyke/misc/any", any(any_misc))
        .route("/other/*path", get(other_dispatch).post(other_dispatch))
        .layer(middleware::from_fn_with_state(auth.clone(), require_token));

    /* A block comment that mentions "/iyke/in-a-block-comment". */
    let open = Router::new()
        .route("/iyke/open/query", post(open_query))
        .route("/", get(root_handler));

    let note = "a string that mentions /iyke/ only in passing";
    let tick = '"';

    authed.merge(open)
}
