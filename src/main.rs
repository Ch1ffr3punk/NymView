use eframe::egui;

mod mixnet_browser;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 800.0])
            .with_min_inner_size([500.0, 600.0])
            .with_title("NymView"),
        ..Default::default()
    };

    eframe::run_native(
        "NymView",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            Ok(Box::new(mixnet_browser::NymMixnetBrowser::new()))
        }),
    )
}
