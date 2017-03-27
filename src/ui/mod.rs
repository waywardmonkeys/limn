pub mod graph;

use backend::gfx::G2d;
use backend::glyph::GlyphCache;
use backend::window::Window;

use std::any::{Any, TypeId};

use cassowary::strength::*;

use graphics;
use graphics::Context;

use widget::WidgetBuilder;
use widget::WidgetBuilderCore;
use layout::solver::LimnSolver;
use layout::LayoutVars;
use util::{self, Point, Rectangle, Dimensions};
use resources::WidgetId;
use color::*;
use event::Target;

use ui::graph::WidgetGraph;
use event::Queue;

pub struct Ui {
    pub graph: WidgetGraph,
    pub solver: LimnSolver,
    queue: Queue,
    glyph_cache: GlyphCache,
    redraw: u32,
    should_close: bool,
    debug_draw_bounds: bool,
}

impl Ui {
    pub fn new(window: &mut Window, queue: &Queue) -> Self {
        Ui {
            graph: WidgetGraph::new(),
            solver: LimnSolver::new(queue.clone()),
            queue: queue.clone(),
            glyph_cache: GlyphCache::new(&mut window.context.factory, 512, 512),
            redraw: 2,
            should_close: false,
            debug_draw_bounds: false,
        }
    }
    pub fn close(&mut self) {
        self.should_close = true;
    }
    pub fn should_close(&self) -> bool {
        self.should_close
    }
    pub fn set_debug_draw_bounds(&mut self, debug_draw_bounds: bool) {
        self.debug_draw_bounds = debug_draw_bounds;
        self.redraw = 1;
    }
    pub fn resize_window_to_fit(&mut self, window: &Window) {
        let window_dims = self.get_root_dims();
        window.window.set_inner_size(window_dims.width as u32, window_dims.height as u32);
    }
    pub fn set_root(&mut self, mut root_widget: WidgetBuilder) {
        root_widget.set_debug_name("root");
        root_widget.layout().top_left(Point { x: 0.0, y: 0.0 });
        {
            let ref root_vars = root_widget.layout().vars;
            self.solver.update_solver(|solver| {
                solver.add_edit_variable(root_vars.right, STRONG).unwrap();
                solver.add_edit_variable(root_vars.bottom, STRONG).unwrap();
            });
        }
        self.graph.root_id = root_widget.id();
        self.add_widget(root_widget, None);
    }
    pub fn get_root_dims(&mut self) -> Dimensions {
        let root = self.graph.get_root();
        let mut dims = root.layout.get_dims();
        // use min size to prevent window size from being set to 0 (X crashes)
        dims.width = f64::max(100.0, dims.width);
        dims.height = f64::max(100.0, dims.height);
        dims
    }
    pub fn window_resized(&mut self, window_dims: Dimensions) {
        let root = self.graph.get_root();
        self.solver.update_solver(|solver| {
            solver.suggest_value(root.layout.right, window_dims.width).unwrap();
            solver.suggest_value(root.layout.bottom, window_dims.height).unwrap();
        });
        self.redraw = 2;
    }

    pub fn redraw(&mut self) {
        self.redraw = 2;
    }
    pub fn draw_if_needed(&mut self, window: &mut Window) {
        if self.redraw > 0 {
            window.draw_2d(|context, graphics| {
                graphics::clear([0.8, 0.8, 0.8, 1.0], graphics);
                self.draw(context, graphics);
            });
            self.redraw -= 1;
        }
    }
    pub fn draw(&mut self, context: Context, graphics: &mut G2d) {
        let crop_to = Rectangle::new_from_pos_dim(Point::zero(), Dimensions::max());
        let id = self.graph.root_id;
        self.draw_node(context, graphics, id, crop_to);
        if self.debug_draw_bounds {
            let root_id = self.graph.root_id;
            let mut dfs = self.graph.dfs(root_id);
            while let Some(widget_id) = dfs.next(&self.graph.graph) {
                let widget = self.graph.get_widget(widget_id).unwrap();
                let color = widget.debug_color.unwrap_or(GREEN);
                let bounds = widget.layout.bounds();
                util::draw_rect_outline(bounds, color, context, graphics);
            }
        }
    }
    pub fn draw_node(&mut self,
                     context: Context,
                     graphics: &mut G2d,
                     widget_id: WidgetId,
                     crop_to: Rectangle) {

        let crop_to = {
            let ref mut widget = self.graph.get_widget(widget_id).unwrap();
            widget.draw(crop_to, &mut self.glyph_cache, context, graphics);
            util::crop_rect(crop_to, widget.layout.bounds())
        };

        if !crop_to.no_area() {
            let children: Vec<WidgetId> = self.graph.children(widget_id).collect(&self.graph.graph);
            // need to iterate backwards to draw in correct order, because
            // petgraph neighbours iterate in reverse order of insertion, not sure why
            for child_index in children.iter().rev() {
                let child_index = child_index.clone();
                self.draw_node(context, graphics, child_index, crop_to);
            }
        }
    }

    pub fn add_widget(&mut self,
                      mut widget: WidgetBuilder,
                      parent_id: Option<WidgetId>) {

        if let Some(parent_id) = parent_id {
            if let Some(parent) = self.graph.get_widget(parent_id) {
                if parent.bound_children {
                    widget.layout().bound_by(&parent.layout);
                }
            }
        }
        let (children, constraints, widget) = widget.build();
        self.solver.add_widget(&widget.widget, constraints);

        let id = widget.widget.id;
        let layout = widget.widget.layout.clone();
        self.graph.add_widget(widget, parent_id);
        self.queue.push(Target::Widget(id), WidgetAttachedEvent);
        if let Some(parent_id) = parent_id {
            self.queue.push(Target::Widget(parent_id), ChildAttachedEvent(id, layout));
        }
        self.redraw();
        for child in children {
            self.add_widget(child, Some(id));
        }
    }

    pub fn remove_widget(&mut self, widget_id: WidgetId) {
        self.queue.push(Target::Widget(widget_id), WidgetDetachedEvent);
        if let Some(widget) = self.graph.remove_widget(widget_id) {
            self.redraw();
            self.solver.remove_widget(&widget.widget.layout);
        }
    }

    pub fn widget_under_cursor(&mut self, point: Point) -> Option<WidgetId> {
        // first widget found is the deepest, later will need to have z order as ordering
        self.graph.widgets_under_cursor(point).next(&mut self.graph.graph)
    }

    fn handle_widget_event(&mut self,
                           widget_id: WidgetId,
                           type_id: TypeId,
                           data: &Box<Any + Send>) -> bool
    {
        if let Some(widget_container) = self.graph.get_widget_container(widget_id) {
            let handled = widget_container.trigger_event(type_id,
                                                     data,
                                                     &mut self.queue,
                                                     &mut self.solver);
            if widget_container.widget.has_updated {
                self.redraw = 2;
                widget_container.widget.has_updated = false;
            }
            handled
        } else {
            false
        }
    }

    pub fn handle_event(&mut self,
                        address: Target,
                        type_id: TypeId,
                        data: &Box<Any + Send>) {
        match address {
            Target::Widget(id) => {
                self.handle_widget_event(id, type_id, data);
            }
            Target::Child(id) => {
                if let Some(child_id) = self.graph.children(id).next(&self.graph.graph) {
                    self.handle_widget_event(child_id, type_id, data);
                }
            }
            Target::SubTree(id) => {
                let mut dfs = self.graph.dfs(id);
                while let Some(widget_id) = dfs.next(&self.graph.graph) {
                    self.handle_widget_event(widget_id, type_id, data);
                }
            }
            Target::BubbleUp(id) => {
                // bubble up event from widget, until either it reaches the root, or some widget handles it
                let mut maybe_id = Some(id);
                while let Some(id) = maybe_id {
                    let handled = self.handle_widget_event(id, type_id, data);
                    maybe_id = if handled { None } else { self.graph.parent(id) };
                }
            }
            _ => ()
        }
    }
}
pub struct WidgetAttachedEvent;
pub struct WidgetDetachedEvent;
pub struct ChildAttachedEvent(pub WidgetId, pub LayoutVars);

pub struct EventArgs<'a> {
    pub ui: &'a mut Ui,
    pub queue: &'a mut Queue,
}

pub trait EventHandler<T> {
    fn handle(&mut self, event: &T, args: EventArgs);
}

pub struct RedrawEvent;

pub struct RedrawHandler;
impl EventHandler<RedrawEvent> for RedrawHandler {
    fn handle(&mut self, _: &RedrawEvent, args: EventArgs) {
        args.ui.redraw();
    }
}
