use std::collections::BTreeMap;

use eframe::egui::{self, RichText};

use crate::app::MailmanApp;
use crate::domain::{method_color, non_empty_trimmed};

use super::shared::HandCursor;
use super::theme;

#[derive(Clone)]
struct SidebarRequestItem {
    endpoint_id: String,
    saved_endpoint_id: Option<String>,
    tab_id: Option<String>,
    method: String,
    name: String,
    collection: String,
    folder_path: String,
    is_dirty: bool,
}

impl MailmanApp {
    pub(in crate::app) fn render_endpoints_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("endpoints")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Requests").strong().size(14.0));

                    ui.add_space(4.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Delete All").color(theme::MUTED).size(11.0),
                            )
                            .frame(false),
                        )
                        .cursor_hand()
                        .clicked()
                    {
                        self.delete_all_requests();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Save").size(12.0))
                                    .fill(egui::Color32::TRANSPARENT),
                            )
                            .on_hover_text("Save all dirty tabs")
                            .cursor_hand()
                            .clicked()
                        {
                            match self.save_all_dirty_request_tabs() {
                                Ok(saved) => {
                                    self.status_line = if saved == 0 {
                                        "No dirty request tabs to save.".to_owned()
                                    } else {
                                        format!("Saved {saved} request tab(s).")
                                    };
                                }
                                Err(err) => {
                                    self.status_line = err;
                                }
                            }
                        }
                        ui.separator();
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("-").color(theme::MUTED).size(14.0),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Delete active request")
                            .cursor_hand()
                            .clicked()
                        {
                            self.delete_selected_endpoint();
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+").color(super::theme::ACCENT).size(16.0),
                                )
                                .frame(false),
                            )
                            .on_hover_text("New request tab")
                            .cursor_hand()
                            .clicked()
                        {
                            self.add_endpoint();
                        }
                    });
                });

                ui.separator();

                let request_items = self.sidebar_request_items();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let selected_location = if self.expand_to_selection {
                        self.active_request_tab().map(|tab| {
                            let collection = non_empty_trimmed(&tab.draft.collection)
                                .unwrap_or("General")
                                .to_owned();
                            let folder = tab.draft.folder_path.trim().to_owned();
                            (collection, folder)
                        })
                    } else {
                        None
                    };

                    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<SidebarRequestItem>>> =
                        BTreeMap::new();
                    for item in request_items {
                        grouped
                            .entry(item.collection.clone())
                            .or_default()
                            .entry(item.folder_path.clone())
                            .or_default()
                            .push(item);
                    }

                    for (collection, folders) in grouped {
                        let coll_open = selected_location
                            .as_ref()
                            .map(|(c, _)| c == &collection)
                            .unwrap_or(false);

                        egui::CollapsingHeader::new(RichText::new(&collection).strong())
                            .id_salt(format!("coll_{collection}"))
                            .open(if coll_open { Some(true) } else { None })
                            .show(ui, |ui| {
                                for (folder, items) in folders {
                                    if folder.is_empty() {
                                        for item in items {
                                            self.render_sidebar_request_item(ui, &item);
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
                                            for item in items {
                                                self.render_sidebar_request_item(ui, &item);
                                            }
                                        });
                                    }
                                }
                            });
                    }

                    self.expand_to_selection = false;
                });
            });
    }

    fn render_sidebar_request_item(&mut self, ui: &mut egui::Ui, item: &SidebarRequestItem) {
        let is_selected = self.active_endpoint_id() == Some(item.endpoint_id.as_str());
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!("{:<6}", &item.method))
                    .color(method_color(&item.method))
                    .monospace()
                    .size(11.0),
            );
            let mut title = if item.is_dirty {
                format!("* {}", item.name)
            } else {
                item.name.clone()
            };
            if title.trim().is_empty() {
                title = "Untitled Request".to_owned();
            }
            let text = if is_selected {
                RichText::new(title).strong()
            } else {
                RichText::new(title)
            };
            if ui
                .selectable_label(is_selected, text)
                .cursor_hand()
                .clicked()
            {
                if let Some(tab_id) = item.tab_id.clone() {
                    self.activate_request_tab(Some(tab_id));
                } else if let Some(saved_endpoint_id) = item.saved_endpoint_id.as_ref() {
                    self.open_saved_request_in_tab(saved_endpoint_id);
                }
            }
        });
    }

    fn sidebar_request_items(&self) -> Vec<SidebarRequestItem> {
        let mut items = self
            .saved_endpoints
            .iter()
            .map(|endpoint| SidebarRequestItem {
                endpoint_id: endpoint.id.clone(),
                saved_endpoint_id: Some(endpoint.id.clone()),
                tab_id: None,
                method: endpoint.method.clone(),
                name: endpoint.name.clone(),
                collection: non_empty_trimmed(&endpoint.collection)
                    .unwrap_or("General")
                    .to_owned(),
                folder_path: endpoint.folder_path.trim().to_owned(),
                is_dirty: false,
            })
            .collect::<Vec<_>>();

        for tab in &self.open_request_tabs {
            if let Some(saved_endpoint_id) = tab.saved_endpoint_id.as_ref()
                && let Some(item) = items
                    .iter_mut()
                    .find(|item| item.saved_endpoint_id.as_deref() == Some(saved_endpoint_id))
            {
                item.endpoint_id = tab.draft.id.clone();
                item.tab_id = Some(tab.id.clone());
                item.method = tab.draft.method.clone();
                item.name = tab.draft.name.clone();
                item.collection = non_empty_trimmed(&tab.draft.collection)
                    .unwrap_or("General")
                    .to_owned();
                item.folder_path = tab.draft.folder_path.trim().to_owned();
                item.is_dirty = tab.is_dirty;
                continue;
            }

            items.push(SidebarRequestItem {
                endpoint_id: tab.draft.id.clone(),
                saved_endpoint_id: None,
                tab_id: Some(tab.id.clone()),
                method: tab.draft.method.clone(),
                name: tab.draft.name.clone(),
                collection: non_empty_trimmed(&tab.draft.collection)
                    .unwrap_or("General")
                    .to_owned(),
                folder_path: tab.draft.folder_path.trim().to_owned(),
                is_dirty: tab.is_dirty,
            });
        }

        items.sort_by(|left, right| {
            (
                left.collection.to_lowercase(),
                left.folder_path.to_lowercase(),
                left.name.to_lowercase(),
            )
                .cmp(&(
                    right.collection.to_lowercase(),
                    right.folder_path.to_lowercase(),
                    right.name.to_lowercase(),
                ))
        });
        items
    }
}
