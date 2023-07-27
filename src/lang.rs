use i18n_embed::{
    fluent::{fluent_language_loader, FluentLanguageLoader},
    LanguageLoader, DesktopLanguageRequester, unic_langid::LanguageIdentifier,
};
use once_cell::sync::Lazy;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "./i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: Lazy<FluentLanguageLoader> = Lazy::new(|| {
    let language_loader: FluentLanguageLoader = fluent_language_loader!();
    let requested_languages = DesktopLanguageRequester::requested_languages();
    let fallback_language: &[LanguageIdentifier] = &["en-US".parse().unwrap()];
    let languages: Vec<&LanguageIdentifier> = requested_languages
        .iter()
        .chain(fallback_language.iter())
        .collect();
    language_loader
        .load_languages(&Localizations, &languages)
        .expect("Unable to local i18n languager");
    language_loader
});

#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::lang::LANGUAGE_LOADER, $message_id)
    }};

    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::lang::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}

// Get the `Localizer` to be used for localizing this library.
// pub fn localizer() -> Box<dyn Localizer> {
//     Box::from(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations))
// }