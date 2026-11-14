//! Shared, stateless building blocks styled with [`crate::theme`].
//! Prefer these over ad-hoc divs so every module looks consistent.

mod avatar;
mod badge;
mod banner;
mod button;
mod empty_state;
mod icon_button;
mod toolbar;

pub use avatar::{Avatar, avatar_color, initials};
pub use badge::{Badge, BadgeVariant};
pub use banner::{Banner, BannerKind, toast};
pub use button::{Button, ButtonVariant};
pub use empty_state::EmptyState;
pub use icon_button::IconButton;
pub use toolbar::{Toolbar, toolbar_separator};

/// Full Lucide catalog (e.g. `IconName::Mail`, `IconName::CalendarDays`).
pub use gpui::assets::IconName;
pub use gpui::component::Icon;

use gpui::{App, ClickEvent, Window};

/// Click callback stored by components.
pub(crate) type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
