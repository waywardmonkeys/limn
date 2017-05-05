use std::sync::Mutex;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use linked_hash_map::LinkedHashMap;

use cassowary;
use cassowary::strength;
use cassowary::{Variable, Constraint, Expression};

use resources::WidgetId;
use widget::Widget;
use event::Target;
use ui::Ui;

use layout::{LayoutVars, LayoutUpdate};

/// wrapper around cassowary solver that keeps widgets positions in sync, sends events when layout changes happen
pub struct LimnSolver {
    solver: cassowary::Solver,
    var_map: HashMap<Variable, HashSet<Constraint>>,
    constraint_map: HashMap<Constraint, Vec<Variable>>,
    widget_map: HashMap<Variable, WidgetId>,
    debug_constraint_list: LinkedHashMap<Constraint, ()>, // LinkedHashSet (maintains insertion order)
}

impl LimnSolver {
    pub fn new() -> Self {
        LimnSolver {
            solver: cassowary::Solver::new(),
            var_map: HashMap::new(),
            constraint_map: HashMap::new(),
            widget_map: HashMap::new(),
            debug_constraint_list: LinkedHashMap::new(),
        }
    }
    pub fn add_widget(&mut self, widget: &Widget, layout_update: LayoutUpdate) {
        let ref vars = widget.layout;
        self.widget_map.insert(vars.left, widget.id);
        self.widget_map.insert(vars.top, widget.id);
        self.widget_map.insert(vars.width, widget.id);
        self.widget_map.insert(vars.height, widget.id);

        if let Some(ref debug_name) = widget.debug_name {
            add_debug_var_name(widget.layout.left, &format!("{}.left", debug_name));
            add_debug_var_name(widget.layout.top, &format!("{}.top", debug_name));
            add_debug_var_name(widget.layout.right, &format!("{}.right", debug_name));
            add_debug_var_name(widget.layout.bottom, &format!("{}.bottom", debug_name));
            add_debug_var_name(widget.layout.width, &format!("{}.width", debug_name));
            add_debug_var_name(widget.layout.height, &format!("{}.height", debug_name));
        }
        self.update_from_builder(layout_update);
    }
    pub fn remove_widget(&mut self, widget_vars: &LayoutVars) {
        for var in [widget_vars.left, widget_vars.top, widget_vars.right, widget_vars.bottom].iter() {
            // remove constraints that are relative to this widget from solver
            if let Some(constraint_set) = self.var_map.remove(&var) {
                for constraint in constraint_set {
                    if self.solver.has_constraint(&constraint) {
                        self.debug_constraint_list.remove(&constraint);
                        self.solver.remove_constraint(&constraint).unwrap();
                        // look up other variables that references this constraint,
                        // and remove this constraint from those variables constraint sets
                        if let Some(var_list) = self.constraint_map.get(&constraint) {
                            for var in var_list {
                                if let Some(constraint_set) = self.var_map.get_mut(&var) {
                                    constraint_set.remove(&constraint);
                                }
                            }
                        }
                    }
                }
            }
            self.widget_map.remove(&var);
        }
        self.check_changes();
    }
    pub fn update_solver<F>(&mut self, f: F)
        where F: Fn(&mut cassowary::Solver)
    {
        f(&mut self.solver);
        self.check_changes();
    }

    pub fn has_edit_variable(&mut self, v: &Variable) -> bool {
        self.solver.has_edit_variable(v)
    }
    pub fn has_constraint(&self, constraint: &Constraint) -> bool {
        self.solver.has_constraint(constraint)
    }

    pub fn update_from_builder(&mut self, layout_update: LayoutUpdate) {
        for edit_var in layout_update.edit_vars {
            if !self.solver.has_edit_variable(&edit_var.var) {
                self.solver.add_edit_variable(edit_var.var, edit_var.strength).unwrap();
            }
            self.solver.suggest_value(edit_var.var, edit_var.val).unwrap();
        }
        for constraint in layout_update.constraints {
            self.add_constraint(constraint.clone());
            let var_list = self.constraint_map.entry(constraint.clone()).or_insert(Vec::new());
            for term in &constraint.0.expression.terms {
                let variable = term.variable;
                let constraint_set = self.var_map.entry(variable).or_insert(HashSet::new());
                constraint_set.insert(constraint.clone());
                var_list.push(variable);
            }
        }
        self.check_changes();
    }
    fn add_constraint(&mut self, constraint: Constraint) {
        self.debug_constraint_list.insert(constraint.clone(), ());
        self.solver.add_constraint(constraint.clone()).expect(&format!("Failed to add constraint {}", fmt_constraint(&constraint)));
    }

    fn check_changes(&mut self) {
        let changes = self.solver.fetch_changes();
        if changes.len() > 0 {
            let mut wchanges = Vec::new();
            for &(var, que) in changes {
                if let Some(widget_id) = self.widget_map.get(&var) {
                    wchanges.push((*widget_id, var, que));
                }
            }
            event!(Target::Ui, LayoutChanged(wchanges));
        }
    }
    pub fn debug_constraints(&self) {
        println!("CONSTRAINTS");
        for constraint in self.debug_constraint_list.keys() {
            debug_constraint(constraint);
        }
    }
}

pub struct LayoutChanged(Vec<(WidgetId, Variable, f64)>);

pub fn handle_layout_change(event: &LayoutChanged, ui: &mut Ui) {
    let ref changes = event.0;
    for &(widget_id, var, value) in changes {
        if let Some(widget) = ui.graph.get_widget(widget_id) {
            widget.layout.update_bounds(var, value, &mut widget.bounds);
        }
    }
    // redraw everything when layout changes, for now
    ui.redraw();
}

fn debug_constraint(constraint: &Constraint) {
    println!("{}", fmt_constraint(constraint));
}

pub fn fmt_constraint(constraint: &Constraint) -> String {
    let ref constraint = constraint.0;
    let strength_desc = {
        let stren = constraint.strength;
        if stren < strength::WEAK { "WEAK-" }
        else if stren == strength::WEAK { "WEAK " }
        else if stren < strength::MEDIUM { "WEAK+" }
        else if stren == strength::MEDIUM { "MED  " }
        else if stren < strength::STRONG { "MED+ " }
        else if stren == strength::STRONG { "STR  " }
        else if stren < strength::REQUIRED { "STR+ " }
        else if stren == strength::REQUIRED { "REQD " }
        else { "REQD+" }
    };
    format!("{} {} {} 0", strength_desc, fmt_expression(&constraint.expression), constraint.op)
}

fn fmt_expression(expression: &Expression) -> String {
    let mut out = String::new();
    let mut first = true;
    if expression.constant != 0.0 {
        write!(out, "{}", expression.constant).unwrap();
        first = false;
    }
    for term in expression.terms.iter() {
        let coef = {
            if term.coefficient == 1.0 {
                if first {
                    "".to_owned()
                } else {
                    "+ ".to_owned()
                }
            } else if term.coefficient == -1.0 {
                "- ".to_owned()
            } else if term.coefficient > 0.0 {
                if !first {
                    format!("+ {} * ", term.coefficient)
                } else {
                    format!("{} * ", term.coefficient)
                }
            } else {
                format!("- {} * ", term.coefficient)
            }
        };
        write!(out, " {}{}", coef, fmt_variable(term.variable)).unwrap();

        first = false;
    }
    out
}

fn fmt_variable(variable: Variable) -> String {
    let names = VAR_NAMES.lock().unwrap();
    if let Some(name) = names.get(&variable) {
        format!("{}", name)
    } else {
        format!("var({:?})", variable)
    }
}

lazy_static! {
    pub static ref VAR_NAMES: Mutex<HashMap<Variable, String>> = Mutex::new(HashMap::new());
}
pub fn add_debug_var_name(var: Variable, name: &str) {
    let mut names = VAR_NAMES.lock().unwrap();
    names.insert(var, name.to_owned());
}
