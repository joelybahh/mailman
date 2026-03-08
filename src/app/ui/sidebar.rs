use std::collections::BTreeMap;

use eframe::egui::{self, RichText};

use crate::app::MailmanApp;
use crate::domain::{method_color, non_empty_trimmed};

use super::shared::HandCursor;
use super::theme;

impl MailmanApp {
    pub(in crate::app) fn render_endpoints_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("endpoints")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Requests").strong().size(14.0));

                    // Delete All / confirmation inline, right after the heading
                    ui.add_space(4.0);
                    if !self.confirm_delete_all_requests {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Delete All")
                                        .color(theme::MUTED)
                                        .size(11.0),
                                )
                                .frame(false),
                            )
                            .cursor_hand()
                            .clicked()
                        {
                            self.confirm_delete_all_requests = true;
                        }
                    } else {
                        ui.label(
                            RichText::new("Clear all?")
                                .color(theme::WARNING)
                                .size(11.0),
                        );
                        if ui.small_button("Yes").cursor_hand().clicked() {
                            self.delete_all_requests();
                            self.confirm_delete_all_requests = false;
                        }
                        if ui.small_button("No").cursor_hand().clicked() {
                            self.confirm_delete_all_requests = false;
                        }
                    }

                    // Right side: + − Save
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Save").size(12.0))
                                    .fill(egui::Color32::TRANSPARENT),
                            )
                            .on_hover_text("Save now")
                            .cursor_hand()
                            .clicked()
                        {
                            use std::time::{Duration, Instant};
                            self.last_mutation = Instant::now() - Duration::from_secs(1);
                            self.try_auto_save();
                        }
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("−").color(theme::MUTED).size(14.0),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Delete selected")
                            .cursor_hand()
                            .clicked()
                        {
                            self.delete_selected_endpoint();
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+")
                                        .color(super::theme::ACCENT)
                                        .size(16.0),
                                )
                                .frame(false),
                            )
                            .on_hover_text("New request")
                            .cursor_hand()
                            .clicked()
                        {
                            self.add_endpoint();
                        }
                    });
                });

                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut selection_changed = false;

                    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<usize>>> =
                        BTreeMap::new();
                    for (index, endpoint) in self.endpoints.iter().enumerate() {
                        let collection = non_empty_trimmed(&endpoint.collection)
                            .unwrap_or("General")
                            .to_owned();
                        let folder = endpoint.folder_path.trim().to_owned();
                        grouped
                            .entry(collection)
                            .or_default()
                            .entry(folder)
                            .or_default()
                            .push(index);
                    }

                    // On the very first render after launch, figure out which
                    // collection + folder to auto-expand to reach the restored selection.
                    let expand = self.expand_to_selection;
                    let selected_location = if expand {
                        self.selected_endpoint_id
                            .as_ref()
                            .and_then(|id| self.endpoints.iter().find(|e| &e.id == id))
                            .map(|ep| {
                                let coll = non_empty_trimmed(&ep.collection)
                                    .unwrap_or("General")
                                    .to_owned();
                                let fold = ep.folder_path.trim().to_owned();
                                (coll, fold)
                            })
                    } else {
                        None
                    };

                    for (collection, folders) in grouped {
                        let coll_open = selected_location
                            .as_ref()
                            .map(|(c, _)| c == &collection)
                            .unwrap_or(false);

                        egui::CollapsingHeader::new(RichText::new(&collection).strong())
                            .id_salt(format!("coll_{collection}"))
                            .open(if coll_open { Some(true) } else { None })
                            .show(ui, |ui| {
                                for (folder, indexes) in folders {
                                    if folder.is_empty() {
                                        for endpoint_index in indexes {
                                            let endpoint = &self.endpoints[endpoint_index];
                                            let is_selected = self.selected_endpoint_id.as_deref()
                                                == Some(endpoint.id.as_str());
                                            let endpoint_id = endpoint.id.clone();
                                            let endpoint_method = endpoint.method.clone();
                                            let endpoint_name = endpoint.name.clone();
                                            ui.horizontal(|ui| {
                                                ui.add_space(2.0);
                                                ui.label(
                                                    RichText::new(format!("{:<6}", &endpoint_method))
                                                        .color(method_color(&endpoint_method))
                                                        .monospace()
                                                        .size(11.0),
                                                );
                                                let name = if is_selected {
                                                    RichText::new(&endpoint_name).strong()
                                                } else {
                                                    RichText::new(&endpoint_name)
                                                };
                                                if ui.selectable_label(is_selected, name).cursor_hand().clicked() {
                                                    self.set_selected_endpoint(Some(endpoint_id.clone()));
                                                    selection_changed = true;
                                                }
                                            });
                                        }
                                    } else {
                                        let fold_open = selected_location
                                            .as_ref()
                                            .map(|(c, f)| c == &collection && f == &folder)
                                            .unwrap_or(false);

                                        egui::CollapsingHeader::new(
                                            RichText::new(&folder).color(theme::MUTED),
                                        )
                                        .id_salt(format!("fold_{collection}_{folder}"))
                                        .open(if fold_open { Some(true) } else { None })
                                        .show(ui, |ui| {
                                            for endpoint_index in indexes {
                                                let endpoint = &self.endpoints[endpoint_index];
                                                let is_selected =
                                                    self.selected_endpoint_id.as_deref()
                                                        == Some(endpoint.id.as_str());
                                                let endpoint_id = endpoint.id.clone();
                                                let endpoint_method = endpoint.method.clone();
                                                let endpoint_name = endpoint.name.clone();
                                                ui.horizontal(|ui| {
                                                    ui.add_space(2.0);
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{:<6}",
                                                            &endpoint_method
                                                        ))
                                                        .color(method_color(&endpoint_method))
                                                        .monospace()
                                                        .size(11.0),
                                                    );
                                                    let name = if is_selected {
                                                        RichText::new(&endpoint_name).strong()
                                                    } else {
                                                        RichText::new(&endpoint_name)
                                                    };
                                                    if ui
                                                        .selectable_label(is_selected, name)
                                                        .cursor_hand()
                                                        .clicked()
                                                    {
                                                        self.set_selected_endpoint(Some(
                                                            endpoint_id.clone(),
                                                        ));
                                                        selection_changed = true;
                                                    }
                                                });
                                            }
                                        });
                                    }
                                }
                            });
                    }

                    // Only expand once — user controls state from here on.
                    self.expand_to_selection = false;

                    if selection_changed {
                        self.mark_dirty();
                    }
                });
            });
    }
}
