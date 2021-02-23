mod dir;

use druid::{
    kurbo::{Circle, CircleSegment, Shape},
    piet::{Text, TextLayout, TextLayoutBuilder},
    widget::{Flex, Label},
};
use druid::{
    AppDelegate, AppLauncher, Color, Command, Data, DelegateCtx, Env, Event, ExtEventSink, Handled,
    LifeCycle, PaintCtx, Point, RenderContext, Selector, Target, Widget, WidgetExt, WindowDesc,
};
use std::{
    collections::{hash_map::HashMap, VecDeque},
    path::PathBuf,
    sync::{
        mpsc::{channel, Sender},
        Arc,
    },
    thread::JoinHandle,
    time::Instant,
};

const SET_SCANNING: Selector<String> = Selector::new("set_scanning");
const SET_ENTRY: Selector<Arc<Entry>> = Selector::new("set_entry");
const SET_ERROR: Selector<String> = Selector::new("set_error");
const NOTIFY_SCAN_FINISH: Selector<()> = Selector::new("notify_scan_finish");
const REQUEST_SCAN: Selector<PathBuf> = Selector::new("request_scan");
const REQUEST_REFRESH: Selector<()> = Selector::new("request_refresh");
const REQUEST_OPEN_DIALOG: Selector<()> = Selector::new("request_open_dialog");

const MAX_COUNT: usize = 20;
const MAX_DEPTH: u8 = 10;

const MIN_SWEEP_SIZE: f64 = 0.01;

#[derive(Clone, Data)]
struct Entry {
    #[data(same_fn = "PartialEq::eq")]
    path: PathBuf,
    size: u64,
    children: Arc<Vec<Arc<Entry>>>,
}

#[derive(Clone, Data)]
struct AppState {
    #[data(same_fn = "PartialEq::eq")]
    current_dir: PathBuf,
    entry: Arc<Entry>,
    total: u64,
    #[data(same_fn = "PartialEq::eq")]
    scanning_dir: Option<String>,
    error: String,
    header: String, // label
    expand: String, // label
    status: String, // label
}

fn open_directory_dialog() -> Option<PathBuf> {
    match tinyfiledialogs::select_folder_dialog("", "") {
        Some(result) => Some(PathBuf::from(result)),
        None => None,
    }
}

fn main() {
    let selected_dir = open_directory_dialog();
    if selected_dir.is_none() {
        return;
    }

    let window = WindowDesc::new(ui_builder())
        .window_size((960.0, 540.0))
        .title("Rustitude");
    let launcher = AppLauncher::with_window(window);

    let data = AppState {
        current_dir: selected_dir.unwrap(),
        entry: Arc::new(Entry {
            children: Arc::new(Vec::new()),
            path: PathBuf::new(),
            size: 0u64,
        }),
        total: 0u64,
        header: String::new(),
        expand: String::new(),
        status: String::new(),
        scanning_dir: None,
        error: String::new(),
    };

    launcher
        .delegate(Delegate {})
        .launch(data)
        .expect("launch failed");
}

struct Delegate {}
impl AppDelegate<AppState> for Delegate {
    fn event(
        &mut self,
        ctx: &mut DelegateCtx,
        _window_id: druid::WindowId,
        event: Event,
        _data: &mut AppState,
        _env: &Env,
    ) -> Option<Event> {
        match &event {
            Event::KeyDown(v) => {
                if v.key == druid::keyboard_types::Key::F5 {
                    ctx.get_external_handle()
                        .submit_command(REQUEST_REFRESH, (), Target::Auto)
                        .unwrap();
                }
            }
            _ => {}
        }
        Some(event)
    }

    fn command(
        &mut self,
        _ctx: &mut DelegateCtx,
        _target: Target,
        cmd: &Command,
        data: &mut AppState,
        _env: &Env,
    ) -> Handled {
        if let Some(value) = cmd.get(REQUEST_SCAN) {
            data.current_dir = value.clone();
            data.header = String::new();
        } else if let Some(value) = cmd.get(SET_ENTRY) {
            data.entry = Arc::from(value.clone());
        } else if let Some(value) = cmd.get(SET_SCANNING) {
            data.scanning_dir = Some(value.clone());
            data.status = format!("Scanning {}", value);
        } else if let Some(_) = cmd.get(NOTIFY_SCAN_FINISH) {
            data.scanning_dir = None;
            data.status = format!("Scan of {}", data.current_dir.display());
            data.header = String::from("Press F5 to refresh");
        } else if let Some(value) = cmd.get(SET_ERROR) {
            data.error = value.clone();
            data.status = format!("Error {}", data.error);
        }
        Handled::No
    }

    fn window_added(
        &mut self,
        _id: druid::WindowId,
        _data: &mut AppState,
        _env: &Env,
        _ctx: &mut DelegateCtx,
    ) {
    }

    fn window_removed(
        &mut self,
        _id: druid::WindowId,
        _data: &mut AppState,
        _env: &Env,
        _ctx: &mut DelegateCtx,
    ) {
    }
}

struct Updater {
    handle: Option<JoinHandle<()>>,
    sender: Option<Sender<bool>>,
}

impl Updater {
    pub fn new() -> Self {
        Updater {
            handle: None,
            sender: None,
        }
    }

    fn stop_worker(&mut self) {
        if let Some(x) = self.handle.take() {
            let result = self.sender.take().unwrap().send(true);
            if let Err(x) = result {
                println!("failed to send({}).", x.to_string());
            }
            x.join().unwrap();
        }
    }

    fn start_worker(&mut self, sink: ExtEventSink, path: PathBuf) {
        let (tx, rx) = channel();

        fn collect(
            path: PathBuf,
            cache: &HashMap<String, Vec<(String, u64)>>,
            count: usize,
            depth: u8,
        ) -> Vec<Arc<Entry>> {
            // println!("generate_entries depth={} path={}", depth, path.clone().display());

            if depth > MAX_DEPTH {
                return Vec::new();
            }

            let c = cache.get(path.to_str().unwrap());
            if c.is_none() {
                // println!("cache(key) not found.");
                return Vec::new();
            }

            let mut filtered = c.unwrap().clone();
            filtered.sort_by(|a, b| b.1.cmp(&a.1));

            filtered
                .iter()
                .take(count)
                .filter_map(|v| {
                    let p = PathBuf::from(v.0.clone());
                    if p == path {
                        return None;
                    }

                    let children = if p.is_dir() {
                        Arc::new(collect(p.clone(), cache, count, depth + 1))
                    } else {
                        Arc::new(Vec::new())
                    };

                    let entry = Entry {
                        path: p.clone(),
                        size: v.1,
                        children: children,
                    };
                    Some(Arc::new(entry))
                })
                .collect()
        }

        let handle = std::thread::spawn(move || {
            let start = path.clone();
            println!("starting worker thread for {}.", start.display());

            let mut total: u64 = 0;
            let mut count: u64 = 0;
            const NOTIFY_INTERVAL: u64 = 300;

            let mut cache: HashMap<String, Vec<(String, u64)>> = HashMap::new();
            cache.reserve(100000);

            let now0 = Instant::now();
            let result = dir::get_directory_size_recursive(
                path.as_path(),
                &mut |parent, path, is_dir, size| {
                    let data = rx.try_recv();
                    if data.unwrap_or(false) {
                        return Ok(false);
                    }

                    if is_dir {
                        cache
                            .entry(parent.into())
                            .or_insert_with(Vec::new)
                            .push((path.into(), size));
                        // println!("added cache(dir) parent={} path={} size={}", parent, path.display(), size);

                        count += 1;
                        if count % NOTIFY_INTERVAL == 0 {
                            let entry = Entry {
                                path: start.clone(),
                                size: total,
                                children: Arc::new(collect(start.clone(), &cache, MAX_COUNT, 0)),
                            };
                            sink.submit_command(SET_ENTRY, Arc::from(entry), Target::Auto)
                                .unwrap();
                            sink.submit_command(SET_SCANNING, path.to_string(), Target::Auto)
                                .unwrap();
                        }
                    } else {
                        total += size;
                        cache
                            .entry(parent.into())
                            .or_insert_with(Vec::new)
                            .push((path.into(), size));
                        // println!("added cache(file) parent={} path={} size={}", parent, path.display(), size);
                    }

                    Ok(true)
                },
            );
            println!("elapsed0 = {}", now0.elapsed().as_millis());

            let now1 = Instant::now();
            let entry = Entry {
                path: start.clone(),
                size: total,
                children: Arc::new(collect(start.clone(), &cache, MAX_COUNT, 0)),
            };
            sink.submit_command(SET_ENTRY, Arc::from(entry), Target::Auto)
                .unwrap();
            sink.submit_command(NOTIFY_SCAN_FINISH, (), Target::Auto)
                .unwrap();
            println!("elapsed1 = {}", now1.elapsed().as_millis());

            if let Err(err) = result {
                sink.submit_command(
                    SET_ERROR,
                    format!("Error: {}", err.to_string()),
                    Target::Auto,
                )
                .unwrap();
            }
        });

        self.handle = Some(handle);
        self.sender = Some(tx);
    }
}

impl Widget<AppState> for Updater {
    fn event(&mut self, ctx: &mut druid::EventCtx, event: &Event, data: &mut AppState, _env: &Env) {
        match event {
            Event::Command(cmd) => {
                if let Some(_value) = cmd.get(REQUEST_SCAN) {
                    self.stop_worker();
                    self.start_worker(ctx.get_external_handle(), data.current_dir.clone());
                    let title = format!("Rustitude - {}", data.current_dir.display());
                    ctx.window().set_title(title.as_str());
                } else if let Some(_value) = cmd.get(REQUEST_REFRESH) {
                    self.stop_worker();
                    self.start_worker(ctx.get_external_handle(), data.current_dir.clone());
                } else if let Some(_) = cmd.get(REQUEST_OPEN_DIALOG) {
                    let handle = ctx.get_external_handle();
                    let current_dir = data.current_dir.clone();
                    std::thread::spawn(move || {
                        let result = open_directory_dialog();
                        if let Some(dir) = result {
                            if dir != current_dir {
                                handle
                                    .submit_command(REQUEST_SCAN, dir, Target::Auto)
                                    .unwrap();
                            }
                        }
                    });
                }
            }
            _ => {}
        }
    }

    fn lifecycle(
        &mut self,
        ctx: &mut druid::LifeCycleCtx,
        event: &druid::LifeCycle,
        data: &AppState,
        _env: &Env,
    ) {
        match event {
            LifeCycle::WidgetAdded => {
                self.start_worker(ctx.get_external_handle(), data.current_dir.clone());
            }
            _ => {}
        }
    }

    fn update(
        &mut self,
        _ctx: &mut druid::UpdateCtx,
        _old_data: &AppState,
        _data: &AppState,
        _env: &Env,
    ) {
    }

    fn layout(
        &mut self,
        _ctx: &mut druid::LayoutCtx,
        _bc: &druid::BoxConstraints,
        _data: &AppState,
        _env: &Env,
    ) -> druid::Size {
        druid::Size::new(0.0, 0.0)
    }

    fn paint(&mut self, _ctx: &mut PaintCtx, _data: &AppState, _env: &Env) {}
}

struct Segment {
    entry: Arc<Entry>,
    circle_segment: CircleSegment,
    // path: String,
    is_dir: bool,
}
struct Chart {
    size: String,
    cursor: Point,
    hovered_entry: Option<Arc<Entry>>,
    hovered_center: bool,
    expand: VecDeque<Arc<Entry>>,
    segments: Vec<Segment>,
    accept: bool,
}

impl Chart {
    pub fn new() -> Self {
        Chart {
            size: String::new(),
            cursor: Point::new(0.0, 0.0),
            hovered_entry: None,
            hovered_center: false,
            expand: VecDeque::new(),
            segments: Vec::new(),
            accept: false,
        }
    }

    fn refresh_segments(&mut self, entry: Arc<Entry>) {
        const OUTER: f64 = 60.0;
        const INNER: f64 = 40.0;
        const START: f64 = 0.0;
        const END: f64 = 2.0 * std::f64::consts::PI;
        self.segments = self.create_segments_recursive(entry.clone(), OUTER, INNER, START, END);
    }

    fn create_segments_recursive(
        &mut self,
        entry: Arc<Entry>,
        outer: f64,
        inner: f64,
        start: f64,
        end: f64,
    ) -> Vec<Segment> {
        let total: u64 = entry.size;
        let mut result: Vec<Segment> = Vec::new();
        let mut pos: f64 = start;

        for v in entry.children.iter() {
            let sweep = v.size as f64 / total as f64 * (end - start);
            if sweep < MIN_SWEEP_SIZE {
                continue;
            }

            let circle_segment =
                CircleSegment::new(druid::Point::new(0.0, 0.0), outer, inner, pos, sweep);
            result.push(Segment {
                entry: v.clone(),
                // path: v.path.to_str().unwrap().into(),
                is_dir: v.path.is_dir(),
                circle_segment: circle_segment,
            });

            if !v.children.is_empty() {
                let mut children: Vec<Segment> = self.create_segments_recursive(
                    v.clone(),
                    outer + 20.0,
                    inner + 20.0,
                    pos,
                    pos + sweep,
                );
                result.append(&mut children);
            }

            pos += sweep;
        }

        return result;
    }

    fn is_hovered_center(&self) -> bool {
        return self.hovered_center;
    }

    fn is_hovered_child(&self) -> bool {
        return self.hovered_entry.is_some();
    }

    fn format_size(&self, value: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;
        const TB: u64 = GB * 1024;
        const PB: u64 = TB * 1024;

        let size = value.to_owned();
        if size < MB {
            format!("{:.02} KB", size as f64 / KB as f64)
        } else if size < GB {
            format!("{:.02} MB", size as f64 / MB as f64)
        } else if size < TB {
            format!("{:.02} GB", size as f64 / GB as f64)
        } else if size < PB {
            format!("{:.02} TB", size as f64 / TB as f64)
        } else {
            format!("{:.02} PB", size as f64 / PB as f64)
        }
    }
}

impl Widget<AppState> for Chart {
    fn event(&mut self, ctx: &mut druid::EventCtx, event: &Event, data: &mut AppState, _env: &Env) {
        match event {
            Event::MouseUp(v) => {
                if self.accept {
                    if v.button.is_left() {
                        if let Some(v) = &self.hovered_entry.as_ref() {
                            opener::open(&v.path).unwrap();
                        }
                    } else if v.button.is_right() {
                        if self.is_hovered_center() {
                            self.expand.pop_front();
                            let entry = if let Some(entry) = self.expand.front() {
                                entry.clone()
                            } else {
                                data.entry.clone()
                            };
                            self.refresh_segments(entry.clone());

                            ctx.request_paint();
                        } else if self.is_hovered_child() {
                            self.expand
                                .push_front(self.hovered_entry.as_ref().unwrap().clone());
                            self.refresh_segments(self.expand.front().unwrap().clone());

                            ctx.request_paint();
                        }
                    }
                }
            }
            Event::MouseMove(v) => {
                self.cursor = v.pos;

                if self.accept {
                    if self.is_hovered_center() {
                        if let Some(expand) = self.expand.front() {
                            data.expand = String::from("Right-click to go back");
                            data.status = format!("Click to locate {}", expand.path.display());
                            self.size = self.format_size(expand.size);
                        } else {
                            data.expand = String::new();
                            data.status = format!("Click to locate {}", data.current_dir.display());
                            self.size = self.format_size(data.entry.size);
                        }
                    } else if self.is_hovered_child() {
                        data.expand = String::from("Right-click to expand");
                        if let Some(entry) = self.hovered_entry.clone() {
                            data.status = format!("Click to locate {}", entry.path.display());
                            self.size = self.format_size(entry.size);
                        }
                    } else {
                        data.expand = String::new();

                        if data.scanning_dir.is_none() {
                            if let Some(expand) = self.expand.front() {
                                data.status = format!("Scan of {}", expand.path.display());
                                self.size = self.format_size(expand.size);
                            } else {
                                data.status = format!("Scan of {}", data.current_dir.display());
                                self.size = self.format_size(data.entry.size);
                            }
                        }
                    }
                }

                ctx.request_paint();
            }
            Event::Command(cmd) => {
                if let Some(entry) = cmd.get(SET_ENTRY) {
                    self.refresh_segments(entry.clone());
                    self.size = self.format_size(entry.size);
                } else if let Some(_) = cmd.get(REQUEST_SCAN) {
                    self.segments.clear();
                    self.expand.clear();
                    self.size.clear();
                    self.hovered_entry = None;
                    self.accept = false;
                } else if let Some(_) = cmd.get(NOTIFY_SCAN_FINISH) {
                    self.accept = true
                }
            }
            _ => {}
        }
    }

    fn lifecycle(
        &mut self,
        _ctx: &mut druid::LifeCycleCtx,
        _event: &druid::LifeCycle,
        _data: &AppState,
        _env: &Env,
    ) {
    }

    fn update(
        &mut self,
        _ctx: &mut druid::UpdateCtx,
        _old_data: &AppState,
        _data: &AppState,
        _env: &Env,
    ) {
    }

    fn layout(
        &mut self,
        _ctx: &mut druid::LayoutCtx,
        bc: &druid::BoxConstraints,
        _data: &AppState,
        _env: &Env,
    ) -> druid::Size {
        bc.max()
    }

    fn paint(&mut self, ctx: &mut PaintCtx, data: &AppState, _env: &Env) {
        let brush_bg = ctx.solid_brush(Color::from_rgba32_u32(0xffffffff));
        let brush_stroke = ctx.solid_brush(Color::from_rgba32_u32(0x101010bc));
        let brush_fill_hovered = ctx.solid_brush(Color::from_rgba32_u32(0x2f6fffff));
        let brush_fill_dir = ctx.solid_brush(Color::from_rgba32_u32(0x4faaffff));
        let brush_fill_file = ctx.solid_brush(Color::from_rgba32_u32(0xc4e0ffff));
        let text_color = Color::from_rgba32_u32(0x000000ff);

        let bounds = ctx.size().to_rect();
        ctx.fill(bounds, &brush_bg);

        self.hovered_entry = None;

        let center = bounds.center();
        let circle_path = Circle::new(center, 40.0);
        ctx.stroke(&circle_path, &brush_stroke, 1.5);
        if circle_path.contains(self.cursor) {
            self.hovered_center = true;
            if let Some(expand) = self.expand.front() {
                self.hovered_entry = Some(expand.clone());
            } else {
                self.hovered_entry = Some(data.entry.clone());
            }

            ctx.fill(&circle_path, &brush_fill_hovered);
        } else {
            self.hovered_center = false;
            ctx.fill(&circle_path, &brush_fill_dir);
        }

        let text = if data.scanning_dir.is_some() {
            String::from("Scanning ...")
        } else {
            self.size.clone()
        };
        let layout = ctx
            .text()
            .new_text_layout(text)
            .text_color(text_color)
            .build()
            .unwrap();
        let size = layout.size();
        let mut pos = bounds.center();
        pos.x -= size.width / 2.0;
        pos.y -= size.height / 2.0;
        ctx.draw_text(&layout, pos);

        ctx.with_save(|ctx| {
            ctx.transform(druid::Affine::translate(druid::Vec2::new(
                center.x, center.y,
            )));

            let dy = (self.cursor.y - center.y) as f64;
            let dx = (self.cursor.x - center.x) as f64;
            let angle = if dy.atan2(dx) < 0.0 {
                std::f64::consts::PI * 2.0 + dy.atan2(dx)
            } else {
                dy.atan2(dx)
            };
            let rx = self.cursor.x - center.x;
            let ry = self.cursor.y - center.y;

            for v in &self.segments {
                // CircleSegment::contains() seems to return wrong result in small segment.

                // let circle_segment_path = v.circle_segment.to_path(0.1);
                // let hovered = circle_segment_path.contains(self.cursor);
                // if hovered {
                //     self.hovered = Some(v.path.clone());
                // }
                let outer = v.circle_segment.outer_radius;
                let inner = v.circle_segment.inner_radius;
                let is_hovered = (rx * rx + ry * ry) <= (outer * outer)
                    && (rx * rx + ry * ry) > (inner * inner)
                    && (angle >= v.circle_segment.start_angle)
                    && (angle < (v.circle_segment.start_angle + v.circle_segment.sweep_angle));

                if is_hovered {
                    self.hovered_entry = Some(v.entry.clone());
                }

                let fill = if is_hovered {
                    &brush_fill_hovered
                } else if v.is_dir {
                    &brush_fill_dir
                } else {
                    &brush_fill_file
                };

                ctx.fill(&v.circle_segment, fill);
                ctx.stroke(&v.circle_segment, &brush_stroke, 1.0);
            }
        });
    }
}

fn ui_builder() -> impl Widget<AppState> {
    let updater = Updater::new();

    let current_dir = Label::new(|data: &AppState, _env: &_| format!("{}", data.header))
        .with_text_color(Color::from_rgba32_u32(0x000000ff))
        .with_text_size(12.0)
        .background(Color::from_rgba32_u32(0xffffffff))
        .expand_width()
        .on_click(
            |ctx: &mut druid::EventCtx, _data: &mut AppState, _env: &Env| {
                let sink = ctx.get_external_handle();
                sink.submit_command(REQUEST_OPEN_DIALOG, (), Target::Auto)
                    .unwrap();
            },
        );

    let paint = Chart::new().expand();

    let expand = Label::new(|data: &AppState, _env: &_| format!("{}", data.expand))
        .with_text_color(Color::from_rgba32_u32(0x000000ff))
        .with_text_size(12.0)
        .background(Color::from_rgba32_u32(0xffffffff))
        .expand_width();

    let status = Label::new(|data: &AppState, _env: &_| format!("{}", data.status))
        .with_text_color(Color::from_rgba32_u32(0x000000ff))
        .with_text_size(12.0)
        .background(Color::from_rgba32_u32(0xffffffff))
        .expand_width();

    let mut col = Flex::column();
    col.add_child(updater);
    col.add_child(current_dir);
    col.add_flex_child(paint, 1.0);
    col.add_child(expand);
    col.add_child(status);

    return col;
}
