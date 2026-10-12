use utoipa::openapi::security::SecurityScheme;
use utoipa::{Modify, OpenApi};

use crate::aiops::routes as ai_routes;
use crate::auth::handlers as auth_handlers;
use crate::cards as card_handlers;
use crate::catalog::handlers as catalog_handlers;
use crate::chat::handlers as chat_handlers;
use crate::heuristic::handlers as heuristic_handlers;
use crate::intelligence::handlers as intelligence_handlers;
use crate::library::handlers as library_handlers;
use crate::lyrics as lyrics_module;
use crate::moderation as moderation_handlers;
use crate::appeals as appeal_handlers;
use crate::parties::handlers as party_handlers;
use crate::playback::handlers as playback_handlers;
use crate::qol as qol_handlers;
use crate::recommendations::handlers as recommendation_handlers;
use crate::search::handlers as search_handlers;
use crate::security::routes as security_routes;
use crate::social::handlers as social_handlers;
use crate::sources::unified_handlers as unified_handlers;
use crate::sources::youtube::admin as youtube_admin;
use crate::streaming::handlers as streaming_handlers;
use crate::tracking::handlers as tracking_handlers;
use crate::tracking::unified_handlers as unified_tracking;
use crate::users::handlers as users_handlers;

use crate::friends as friend_handlers;
use crate::privacy as privacy_handlers;
use crate::sharing as sharing_handlers;
use crate::youtube as youtube_history;

#[derive(OpenApi)]
#[openapi(
    info(title = "Space Media Server", version = "0.1.0",
        description = "Modular media platform: catalog, streaming, watch tracking, chat, parties, plugins."),
    paths(
        auth_handlers::register,
        auth_handlers::login,
        auth_handlers::refresh,
        auth_handlers::logout,
        auth_handlers::logout_all,
        auth_handlers::list_sessions,
        auth_handlers::revoke_session,
        auth_handlers::change_password,
        auth_handlers::forgot_password,
        auth_handlers::reset_password,
        catalog_handlers::list_media,
        catalog_handlers::get_media,
        catalog_handlers::create_media,
        catalog_handlers::update_media,
        catalog_handlers::delete_media,
        catalog_handlers::create_season,
        catalog_handlers::list_episodes,
        catalog_handlers::create_episode,
        catalog_handlers::metadata_search,
        catalog_handlers::import_metadata,
        search_handlers::search,
        search_handlers::discover,
        search_handlers::trending,
        search_handlers::similar_media,
        tracking_handlers::update_progress,
        tracking_handlers::continue_watching,
        tracking_handlers::history,
        tracking_handlers::set_status,
        tracking_handlers::remove_status,
        tracking_handlers::rate_media,
        tracking_handlers::my_ratings,
        tracking_handlers::toggle_favorite,
        tracking_handlers::favorites,
        tracking_handlers::get_history_settings,
        tracking_handlers::set_history_settings,
        streaming_handlers::list_files,
        streaming_handlers::get_file,
        streaming_handlers::stream_file,
        streaming_handlers::get_subtitle,
        streaming_handlers::probe_file_endpoint,
        playback_handlers::create_session,
        playback_handlers::heartbeat,
        playback_handlers::end_session,
        playback_handlers::my_sessions,
        playback_handlers::resume,
        playback_handlers::enqueue_transcode,
        playback_handlers::job_status,
        playback_handlers::cancel_job,
        playback_handlers::hls_master,
        playback_handlers::hls_asset,
        playback_handlers::create_handoff,
        playback_handlers::redeem_handoff,
        playback_handlers::file_markers,
        playback_handlers::set_marker,
        playback_handlers::detect_markers_endpoint,
        playback_handlers::file_chapters,
        chat_handlers::list_conversations,
        chat_handlers::create_conversation,
        chat_handlers::list_messages,
        chat_handlers::send_message,
        chat_handlers::mark_read,
        social_handlers::list_reviews,
        social_handlers::create_review,
        social_handlers::delete_review,
        social_handlers::feed,
        social_handlers::list_comments,
        social_handlers::create_comment,
        social_handlers::toggle_reaction,
        social_handlers::follow_user,
        party_handlers::create_party,
        party_handlers::join_party,
        party_handlers::join_by_code,
        party_handlers::leave_party,
        party_handlers::get_party,
        party_handlers::control,
        party_handlers::current_state,
        party_handlers::events,
        party_handlers::reaction_heatmap,
        intelligence_handlers::vibe_search,
        intelligence_handlers::moods,
        intelligence_handlers::my_taste,
        intelligence_handlers::home,
        intelligence_handlers::collection_items,
        recommendation_handlers::for_me,
        recommendation_handlers::similar_scores,
        heuristic_handlers::engine_status,
        heuristic_handlers::my_profile,
        heuristic_handlers::my_wrapped,
        heuristic_handlers::my_insights,
        heuristic_handlers::my_predictions,
        heuristic_handlers::my_similar,
        heuristic_handlers::wipe_my_data,
        unified_handlers::search_everything,
        unified_handlers::resolve_v2,
        security_routes::authorize_session,
        security_routes::get_policy,
        security_routes::patch_policy,
        security_routes::list_events,
        security_routes::list_grants,
        security_routes::revoke_grant,
        security_routes::revoke_user_grants,
        ai_routes::get_config,
        ai_routes::patch_config,
        ai_routes::analyze,
        ai_routes::apply,
        ai_routes::events,
        ai_routes::status,
        unified_tracking::update_progress,
        unified_tracking::continue_all,
        qol_handlers::completed,
        qol_handlers::toplists,
        qol_handlers::random_pick,
        qol_handlers::upcoming,
        qol_handlers::me_summary,
        qol_handlers::me_export,
        qol_handlers::wipe_history,
        qol_handlers::my_card_status,
        qol_handlers::server_meta,
        qol_handlers::availability,
        moderation_handlers::create_report,
        moderation_handlers::admin_reports,
        moderation_handlers::update_report_status,
        moderation_handlers::accept_report,
        moderation_handlers::reject_report,
        appeal_handlers::create_appeal,
        appeal_handlers::my_appeals,
        appeal_handlers::admin_appeals,
        appeal_handlers::accept_appeal,
        appeal_handlers::reject_appeal,
        moderation_handlers::block_user,
        moderation_handlers::unblock_user,
        moderation_handlers::my_blocks,
        moderation_handlers::mute_user,
        moderation_handlers::unmute_user,
        moderation_handlers::update_my_presence,
        moderation_handlers::batch_presence,
        moderation_handlers::admin_audit_logs,
        card_handlers::activate_card,
        card_handlers::my_cards,
        card_handlers::create_transfer_code,
        card_handlers::merchant_register,
        card_handlers::merchant_inventory,
        card_handlers::purchase_request,
        card_handlers::give_cards,
        card_handlers::redeem_transfer,
        card_handlers::get_billing_config,
        card_handlers::patch_billing_config,
        card_handlers::admin_purchases,
        card_handlers::approve_purchase,
        card_handlers::reject_purchase,
        card_handlers::mint_cards,
        card_handlers::hold_card,
        card_handlers::card_chain,
        card_handlers::ban_merchant,
        card_handlers::my_cards_summary,
        card_handlers::lock_merchant,
        card_handlers::admin_merchants,
        card_handlers::grant_cards_to_user,
        card_handlers::admin_cards_search,
        users_handlers::grant_permission,
        users_handlers::revoke_permission,
        users_handlers::list_user_permissions,
        friend_handlers::send_request,
        friend_handlers::accept_request,
        friend_handlers::decline_request,
        friend_handlers::cancel_request,
        friend_handlers::list_requests,
        friend_handlers::list_friends,
        friend_handlers::remove_friend,
        sharing_handlers::share_media,
        sharing_handlers::incoming_shares,
        sharing_handlers::share_history,
        sharing_handlers::mark_viewed,
        sharing_handlers::delete_incoming_share,
        privacy_handlers::my_privacy,
        privacy_handlers::patch_my_privacy,
        library_handlers::create_playlist,
        library_handlers::my_playlists,
        library_handlers::get_playlist,
        library_handlers::update_playlist,
        library_handlers::delete_playlist,
        library_handlers::list_items,
        library_handlers::add_item,
        library_handlers::replace_items,
        library_handlers::remove_item,
        library_handlers::reorder,
        library_handlers::storage_info,
        lyrics_module::handlers::get_lyrics,
        lyrics_module::handlers::providers,
        lyrics_module::handlers::media_lyrics,
        youtube_history::record,
        youtube_history::list,
        youtube_history::remove_one,
        youtube_history::clear_all,
        youtube_admin::status,
        youtube_admin::heal,
    ),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

pub struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::HttpBuilder;
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                    .build(),
            ),
        );
    }
}
