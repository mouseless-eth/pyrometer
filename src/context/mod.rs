use crate::{
    range::{DynamicRangeSide, Op, RangeElem},
    AnalyzerLike, Builtin, Edge, FunctionNode, FunctionParamNode, FunctionReturnNode, Node,
    NodeIdx,
};
use petgraph::{visit::EdgeRef, Direction};
use solang_parser::pt::{Expression, Loc, Statement};

pub mod var;
pub use var::*;
pub mod exprs;
use exprs::*;

pub mod analyzers;
pub use analyzers::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum ContextEdge {
    // Control flow
    Context,
    Subcontext,
    Call,

    // Context Variables
    Variable,
    InheritedVariable,

    AttrAccess,
    Index,
    IndexAccess,

    // Variable incoming edges
    Assign,
    StorageAssign,
    MemoryAssign,
    Prev,

    // Control flow
    Return,

    // Range analysis
    Range,
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ContextNode(pub usize);
impl ContextNode {
    pub fn associated_fn(&self, analyzer: &(impl AnalyzerLike + Search)) -> Option<FunctionNode> {
        Some(FunctionNode::from(analyzer.search_for_ancestor(
            self.0.into(),
            &Edge::Context(ContextEdge::Context),
        )?))
    }

    pub fn associated_fn_name(&self, analyzer: &(impl AnalyzerLike + Search)) -> String {
        self.associated_fn(analyzer)
            .expect("No associated function for context")
            .name(analyzer)
    }

    pub fn underlying_mut<'a>(&self, analyzer: &'a mut impl AnalyzerLike) -> &'a mut Context {
        match analyzer.node_mut(*self) {
            Node::Context(c) => c,
            e => panic!(
                "Node type confusion: expected node to be Context but it was: {:?}",
                e
            ),
        }
    }

    pub fn underlying<'a>(&self, analyzer: &'a impl AnalyzerLike) -> &'a Context {
        match analyzer.node(*self) {
            Node::Context(c) => c,
            e => panic!(
                "Node type confusion: expected node to be Context but it was: {:?}",
                e
            ),
        }
    }

    pub fn var_by_name(&self, analyzer: &impl AnalyzerLike, name: &str) -> Option<ContextVarNode> {
        analyzer
            .graph()
            .edges_directed((*self).into(), Direction::Incoming)
            .filter(|edge| *edge.weight() == Edge::Context(ContextEdge::Variable))
            .map(|edge| ContextVarNode::from(edge.source()))
            .filter_map(|cvar_node| {
                let cvar = cvar_node.underlying(analyzer);
                if cvar.name == name {
                    Some(cvar_node)
                } else {
                    None
                }
            })
            .take(1)
            .next()
    }

    pub fn vars(&self, analyzer: &impl AnalyzerLike) -> Vec<ContextVarNode> {
        analyzer
            .graph()
            .edges_directed((*self).into(), Direction::Incoming)
            .filter(|edge| *edge.weight() == Edge::Context(ContextEdge::Variable))
            .map(|edge| ContextVarNode::from(edge.source()))
            .collect()
    }

    pub fn latest_var_by_name(
        &self,
        analyzer: &impl AnalyzerLike,
        name: &str,
    ) -> Option<ContextVarNode> {
        if let Some(var) = self.var_by_name(analyzer, name) {
            Some(var.latest_version(analyzer))
        } else {
            None
        }
    }

    pub fn new_tmp(&self, analyzer: &mut impl AnalyzerLike) -> usize {
        let context = self.underlying_mut(analyzer);
        let ret = context.tmp_var_ctr;
        context.tmp_var_ctr += 1;
        ret
    }
}
impl Into<NodeIdx> for ContextNode {
    fn into(self) -> NodeIdx {
        self.0.into()
    }
}

impl From<NodeIdx> for ContextNode {
    fn from(idx: NodeIdx) -> Self {
        ContextNode(idx.index())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Context {
    pub tmp_var_ctr: usize,
    pub loc: Loc,
}

impl Context {
    pub fn new(loc: Loc) -> Self {
        Context {
            tmp_var_ctr: 0,
            loc,
        }
    }
}

impl<T> ContextBuilder for T where T: AnalyzerLike + Sized + ExprParser {}

pub trait ContextBuilder: AnalyzerLike + Sized + ExprParser {
    fn parse_ctx_statement(
        &mut self,
        stmt: &Statement,
        _unchecked: bool,
        parent_ctx: Option<impl Into<NodeIdx> + Clone + Copy>,
    ) where
        Self: Sized,
    {
        use Statement::*;
        // println!("stmt: {:?}", stmt);
        match stmt {
            Block {
                loc,
                unchecked,
                statements,
            } => {
                let ctx = Context::new(*loc);
                let ctx_node = self.add_node(Node::Context(ctx));

                if let Some(parent) = parent_ctx {
                    match self.node(parent) {
                        Node::Function(_) => {
                            self.add_edge(ctx_node, parent, Edge::Context(ContextEdge::Context));
                        }
                        Node::Context(_) => {
                            self.add_edge(ctx_node, parent, Edge::Context(ContextEdge::Subcontext));
                        }
                        _ => {}
                    }
                }

                // optionally add named input and named outputs into context
                if let Some(parent) = parent_ctx.clone() {
                    self.graph()
                        .edges_directed(parent.into(), Direction::Incoming)
                        .filter(|edge| *edge.weight() == Edge::FunctionParam)
                        .map(|edge| FunctionParamNode::from(edge.source()))
                        .collect::<Vec<FunctionParamNode>>()
                        .iter()
                        .for_each(|param_node| {
                            let func_param = param_node.underlying(self);
                            if let Some(cvar) =
                                ContextVar::maybe_new_from_func_param(self, func_param.clone())
                            {
                                let cvar_node = self.add_node(Node::ContextVar(cvar));
                                self.add_edge(
                                    cvar_node,
                                    ctx_node,
                                    Edge::Context(ContextEdge::Variable),
                                );
                            }
                        });

                    self.graph()
                        .edges_directed(parent.into(), Direction::Incoming)
                        .filter(|edge| *edge.weight() == Edge::FunctionReturn)
                        .map(|edge| FunctionReturnNode::from(edge.source()))
                        .collect::<Vec<FunctionReturnNode>>()
                        .iter()
                        .for_each(|ret_node| {
                            let func_ret = ret_node.underlying(self);
                            if let Some(cvar) =
                                ContextVar::maybe_new_from_func_ret(self, func_ret.clone())
                            {
                                let cvar_node = self.add_node(Node::ContextVar(cvar));
                                self.add_edge(
                                    cvar_node,
                                    ctx_node,
                                    Edge::Context(ContextEdge::Variable),
                                );
                            }
                        });
                }

                statements
                    .iter()
                    .for_each(|stmt| self.parse_ctx_statement(stmt, *unchecked, Some(ctx_node)));
            }
            VariableDefinition(_loc, _var_decl, _maybe_expr) => {}
            Assembly {
                loc: _,
                dialect: _,
                flags: _,
                block: _yul_block,
            } => {}
            Args(_loc, _args) => {}
            If(_loc, _cond, _true_body, _maybe_false_body) => {}
            While(_loc, _cond, _body) => {}
            Expression(_loc, expr) => {
                if let Some(parent) = parent_ctx {
                    let expr_nodes = self.parse_ctx_expr(expr, ContextNode::from(parent.into()));
                    if expr_nodes.is_empty() {
                    } else {
                        self.add_edge(expr_nodes[0], parent, Edge::Context(ContextEdge::Call));
                    }
                }
            }
            For(_loc, _maybe_for_start, _maybe_for_middle, _maybe_for_end, _maybe_for_body) => {}
            DoWhile(_loc, _while_stmt, _while_expr) => {}
            Continue(_loc) => {}
            Break(_loc) => {}
            Return(_loc, maybe_ret_expr) => {
                if let Some(ret_expr) = maybe_ret_expr {
                    if let Some(parent) = parent_ctx {
                        let expr_node =
                            self.parse_ctx_expr(ret_expr, ContextNode::from(parent.into()))[0];
                        self.add_edge(expr_node, parent, Edge::Context(ContextEdge::Return));
                    }
                }
            }
            Revert(_loc, _maybe_err_path, _exprs) => {}
            RevertNamedArgs(_loc, _maybe_err_path, _named_args) => {}
            Emit(_loc, _emit_expr) => {}
            Try(_loc, _try_expr, _maybe_returns, _clauses) => {}
            Error(_loc) => {}
        }
    }

    fn parse_ctx_expr(&mut self, expr: &Expression, ctx: ContextNode) -> Vec<NodeIdx> {
        use Expression::*;
        match expr {
            Variable(ident) => self.variable(ident, ctx),
            // literals
            NumberLiteral(loc, int, exp) => self.number_literal(*loc, int, exp),
            AddressLiteral(loc, addr) => self.address_literal(*loc, addr),
            StringLiteral(lits) => lits
                .iter()
                .flat_map(|lit| self.string_literal(lit.loc, &lit.string))
                .collect(),
            BoolLiteral(loc, b) => self.bool_literal(*loc, *b),
            // bin ops
            Add(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Add, false)
            }
            AssignAdd(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Add, true)
            }
            Subtract(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Sub, false)
            }
            AssignSubtract(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Sub, true)
            }
            Multiply(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Mul, false)
            }
            AssignMultiply(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Mul, true)
            }
            Divide(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Div, false)
            }
            AssignDivide(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Div, true)
            }
            Modulo(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Mod, false)
            }
            AssignModulo(loc, lhs_expr, rhs_expr) => {
                self.op_expr(*loc, lhs_expr, rhs_expr, ctx, Op::Mod, true)
            }
            // assign
            Assign(loc, lhs_expr, rhs_expr) => self.assign(*loc, lhs_expr, rhs_expr, ctx),
            // array
            ArraySubscript(_loc, ty_expr, None) => self.array_ty(ty_expr, ctx),
            ArraySubscript(loc, ty_expr, Some(index_expr)) => {
                self.index_into_array(*loc, ty_expr, index_expr, ctx)
            }
            Type(_loc, ty) => {
                if let Some(builtin) = Builtin::try_from_ty(ty.clone()) {
                    if let Some(idx) = self.builtins().get(&builtin) {
                        vec![*idx]
                    } else {
                        let idx = self.add_node(Node::Builtin(builtin.clone()));
                        self.builtins_mut().insert(builtin, idx);
                        vec![idx]
                    }
                } else {
                    todo!("??")
                }
            }
            MemberAccess(loc, member_expr, ident) => {
                self.member_access(*loc, member_expr, ident, ctx)
            }
            // comparator
            Equal(loc, lhs, rhs) => self.cmp(*loc, lhs, Op::Eq, rhs, ctx),
            Less(loc, lhs, rhs) => self.cmp(*loc, lhs, Op::Lt, rhs, ctx),
            More(loc, lhs, rhs) => self.cmp(*loc, lhs, Op::Gt, rhs, ctx),
            LessEqual(loc, lhs, rhs) => self.cmp(*loc, lhs, Op::Lte, rhs, ctx),
            MoreEqual(loc, lhs, rhs) => self.cmp(*loc, lhs, Op::Gte, rhs, ctx),

            FunctionCall(_loc, func_expr, input_exprs) => {
                let func_idx = self.parse_ctx_expr(func_expr, ctx)[0];

                if let Some(func_name) = &FunctionNode::from(func_idx).underlying(self).name {
                    match &*func_name.name {
                        "require" | "assert" => {
                            self.handle_require(input_exprs, ctx);
                            return vec![];
                        }
                        _ => {}
                    }
                }

                let _inputs: Vec<_> = input_exprs
                    .into_iter()
                    .map(|expr| self.parse_ctx_expr(expr, ctx))
                    .collect();

                // todo!("func call")
                vec![func_idx]
            }

            e => todo!("{:?}", e),
        }
    }

    fn assign(
        &mut self,
        loc: Loc,
        lhs_expr: &Expression,
        rhs_expr: &Expression,
        ctx: ContextNode,
    ) -> Vec<NodeIdx> {
        let lhs_cvar = ContextVarNode::from(self.parse_ctx_expr(&lhs_expr, ctx)[0]);
        let rhs_cvar = ContextVarNode::from(self.parse_ctx_expr(rhs_expr, ctx)[0]);

        let (new_lower_bound, new_upper_bound) = if let Some(range) = rhs_cvar.range(self) {
            (range.min, range.max)
        } else {
            (
                RangeElem::Dynamic(rhs_cvar.into(), DynamicRangeSide::Min, loc),
                RangeElem::Dynamic(rhs_cvar.into(), DynamicRangeSide::Max, loc),
            )
        };

        let new_lhs = self.advance_var(lhs_cvar, loc);
        new_lhs.set_range_min(self, new_lower_bound);
        new_lhs.set_range_max(self, new_upper_bound);
        vec![new_lhs.into()]
    }

    fn advance_var(&mut self, cvar_node: ContextVarNode, loc: Loc) -> ContextVarNode {
        let mut new_cvar = cvar_node.underlying(self).clone();
        new_cvar.loc = Some(loc);
        let new_cvarnode = self.add_node(Node::ContextVar(new_cvar));
        self.add_edge(new_cvarnode, cvar_node.0, Edge::Context(ContextEdge::Prev));
        ContextVarNode::from(new_cvarnode)
    }

    fn advance_var_underlying(&mut self, cvar_node: ContextVarNode, loc: Loc) -> &mut ContextVar {
        let mut new_cvar = cvar_node.underlying(self).clone();
        new_cvar.loc = Some(loc);
        let new_cvarnode = self.add_node(Node::ContextVar(new_cvar));
        self.add_edge(new_cvarnode, cvar_node.0, Edge::Context(ContextEdge::Prev));
        ContextVarNode::from(new_cvarnode).underlying_mut(self)
    }
}
