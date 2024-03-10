use std::{collections::HashMap, sync::Arc};

use log::{debug, trace, warn};

use roc_can::{
    def::Def,
    expr::{ClosureData, Declarations, Expr, WhenBranch},
    pattern::{ListPatterns, Pattern, RecordDestruct, TupleDestruct},
    traverse::{walk_decl, walk_def, walk_expr, DeclarationInfo, Visitor},
};
use roc_collections::{MutMap, VecMap};
use roc_load::docs::{DocDef, ModuleDocumentation};
use roc_module::symbol::{Interns, ModuleId, Symbol};
use roc_region::all::{Loc, Position, Region};
use roc_types::{
    subs::{Subs, Variable},
    types::Alias,
};
use tower_lsp::lsp_types::{self, CompletionItem, CompletionItemKind};

use super::{utils::format_var_type, ModulesInfo};
mod formatting;

pub struct CompletionVisitor<'a> {
    position: Position,
    found_decls: Vec<(Symbol, Variable)>,
    pub interns: &'a Interns,
    pub prefix: String,
}

impl Visitor for CompletionVisitor<'_> {
    fn should_visit(&mut self, region: Region) -> bool {
        region.contains_pos(self.position)
    }

    fn visit_expr(&mut self, expr: &Expr, region: Region, var: Variable) {
        if region.contains_pos(self.position) {
            let mut res = self.expression_defs(expr);
            self.found_decls.append(&mut res);

            walk_expr(self, expr, var);
        }
    }

    fn visit_decl(&mut self, decl: DeclarationInfo<'_>) {
        match decl {
            DeclarationInfo::Value { loc_expr, .. }
            | DeclarationInfo::Function {
                loc_body: loc_expr, ..
            }
            | DeclarationInfo::Destructure { loc_expr, .. } => {
                let res = self.decl_to_completion_item(&decl);
                self.found_decls.extend(res);
                if loc_expr.region.contains_pos(self.position) {
                    walk_decl(self, decl);
                };
            }
            _ => {
                walk_decl(self, decl);
            }
        }
    }

    fn visit_def(&mut self, def: &Def) {
        let res = self.extract_defs(def);
        self.found_decls.extend(res);
        walk_def(self, def);
    }
}
impl CompletionVisitor<'_> {
    fn extract_defs(&mut self, def: &Def) -> Vec<(Symbol, Variable)> {
        trace!("Completion begin");
        def.pattern_vars
            .iter()
            .map(|(symbol, var)| (*symbol, *var))
            .collect()
    }
    fn expression_defs(&self, expr: &Expr) -> Vec<(Symbol, Variable)> {
        match expr {
            Expr::When {
                expr_var, branches, ..
            } => self.when_is_expr(branches, expr_var),
            Expr::Closure(ClosureData {
                arguments,
                loc_body,
                ..
            }) => {
                //if we are inside the closure complete it's vars
                if loc_body.region.contains_pos(self.position) {
                    arguments
                        .iter()
                        .flat_map(|(var, _, pat)| self.patterns(&pat.value, var))
                        .collect()
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    ///Extract any variables made available by the branch of a when_is expression that contains `self.position`
    fn when_is_expr(
        &self,
        branches: &[WhenBranch],
        expr_var: &Variable,
    ) -> Vec<(Symbol, Variable)> {
        branches
            .iter()
            .flat_map(
                |WhenBranch {
                     patterns, value, ..
                 }| {
                    if value.region.contains_pos(self.position) {
                        patterns
                            .iter()
                            .flat_map(|pattern| self.patterns(&pattern.pattern.value, expr_var))
                            .collect()
                    } else {
                        vec![]
                    }
                },
            )
            .collect()
    }

    fn record_destructure(&self, destructs: &[Loc<RecordDestruct>]) -> Vec<(Symbol, Variable)> {
        destructs
            .iter()
            .flat_map(|a| match &a.value.typ {
                roc_can::pattern::DestructType::Required
                | roc_can::pattern::DestructType::Optional(_, _) => {
                    vec![(a.value.symbol, a.value.var)]
                }
                roc_can::pattern::DestructType::Guard(var, pat) => self.patterns(&pat.value, var),
            })
            .collect()
    }

    fn tuple_destructure(&self, destructs: &[Loc<TupleDestruct>]) -> Vec<(Symbol, Variable)> {
        destructs
            .iter()
            .flat_map(|a| {
                let (var, pattern) = &a.value.typ;
                self.patterns(&pattern.value, var)
            })
            .collect()
    }

    fn list_pattern(&self, list_elems: &ListPatterns, var: &Variable) -> Vec<(Symbol, Variable)> {
        list_elems
            .patterns
            .iter()
            .flat_map(|a| self.patterns(&a.value, var))
            .collect()
    }
    fn tag_pattern(&self, arguments: &[(Variable, Loc<Pattern>)]) -> Vec<(Symbol, Variable)> {
        arguments
            .iter()
            .flat_map(|(var, pat)| self.patterns(&pat.value, var))
            .collect()
    }

    fn as_pattern(
        &self,
        as_pat: &Pattern,
        as_symbol: Symbol,
        var: &Variable,
    ) -> Vec<(Symbol, Variable)> {
        //Get the variables introduced within the pattern
        let mut patterns = self.patterns(as_pat, var);
        //Add the "as" that wraps the whole pattern
        patterns.push((as_symbol, *var));
        patterns
    }
    ///Returns a list of symbols defined by this pattern.  
    ///`pattern_var`: Variable type of the entire pattern. This will be returned if the pattern turns out to be an identifier
    fn patterns(
        &self,
        pattern: &roc_can::pattern::Pattern,
        pattern_var: &Variable,
    ) -> Vec<(Symbol, Variable)> {
        match pattern {
            roc_can::pattern::Pattern::Identifier(symbol) => {
                if self.is_match(symbol) {
                    vec![(*symbol, *pattern_var)]
                } else {
                    vec![]
                }
            }
            Pattern::AppliedTag { arguments, .. } => self.tag_pattern(arguments),
            Pattern::UnwrappedOpaque { argument, .. } => {
                self.patterns(&argument.1.value, &argument.0)
            }
            Pattern::List {
                elem_var, patterns, ..
            } => self.list_pattern(patterns, elem_var),
            roc_can::pattern::Pattern::As(pat, symbol) => {
                self.as_pattern(&pat.value, *symbol, pattern_var)
            }
            roc_can::pattern::Pattern::RecordDestructure { destructs, .. } => {
                self.record_destructure(destructs)
            }
            roc_can::pattern::Pattern::TupleDestructure { destructs, .. } => {
                self.tuple_destructure(destructs)
            }
            _ => vec![],
        }
    }

    fn is_match(&self, symbol: &Symbol) -> bool {
        symbol.as_str(self.interns).starts_with(&self.prefix)
    }

    fn decl_to_completion_item(&self, decl: &DeclarationInfo) -> Vec<(Symbol, Variable)> {
        match decl {
            DeclarationInfo::Value {
                expr_var, pattern, ..
            } => self.patterns(pattern, expr_var),
            DeclarationInfo::Function {
                expr_var,
                pattern,
                function,
                loc_body,
                ..
            } => {
                let mut out = vec![];
                //Append the function declaration itself for recursive calls
                out.extend(self.patterns(pattern, expr_var));

                if loc_body.region.contains_pos(self.position) {
                    //also add the arguments if we are inside the function
                    let args = function
                        .value
                        .arguments
                        .iter()
                        .flat_map(|(var, _, pat)| self.patterns(&pat.value, var));
                    //We add in the pattern for the function declaration
                    out.extend(args);
                    trace!("Added function args to completion output =:{:#?}", out);
                }
                out
            }
            DeclarationInfo::Destructure {
                loc_pattern,
                expr_var,
                ..
            } => self.patterns(&loc_pattern.value, expr_var),
            DeclarationInfo::Expectation { .. } => vec![],
        }
    }
}

fn get_completions(
    position: Position,
    decls: &Declarations,
    prefix: String,
    interns: &Interns,
) -> Vec<(Symbol, Variable)> {
    let mut visitor = CompletionVisitor {
        position,
        found_decls: Vec::new(),
        interns,
        prefix,
    };
    visitor.visit_decls(decls);
    visitor.found_decls
}

fn make_completion_item(
    subs: &mut Subs,
    module_id: &ModuleId,
    interns: &Interns,
    docs: Option<String>,
    str: String,
    var: Variable,
) -> CompletionItem {
    let type_str = format_var_type(var, subs, module_id, interns);
    let typ = match subs.get(var).content {
        roc_types::subs::Content::Structure(var) => match var {
            roc_types::subs::FlatType::Apply(_, _) => CompletionItemKind::FUNCTION,
            roc_types::subs::FlatType::Func(_, _, _) => CompletionItemKind::FUNCTION,
            roc_types::subs::FlatType::EmptyTagUnion
            | roc_types::subs::FlatType::TagUnion(_, _) => CompletionItemKind::ENUM,
            _ => CompletionItemKind::VARIABLE,
        },
        a => {
            debug!(
                "No specific completionKind for variable type: {:?} defaulting to 'Variable'",
                a
            );
            CompletionItemKind::VARIABLE
        }
    };

    CompletionItem {
        label: str,
        detail: Some(type_str),
        kind: Some(typ),
        documentation: docs.map(|d| {
            lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: d,
            })
        }),
        ..Default::default()
    }
}
/// Walks through declarations that would be accessible from the provided position adding them to a list of completion items until all accessible declarations have been fully explored
pub fn get_completion_items(
    position: Position,
    prefix: String,
    decls: &Declarations,
    subs: &mut Subs,
    module_id: &ModuleId,
    interns: &Interns,
    docs: Option<&ModuleDocumentation>,
    exposed_imports: &[(Symbol, Variable)],
) -> Vec<CompletionItem> {
    let mut completions = get_completions(position, decls, prefix, interns);
    completions.extend(exposed_imports);
    debug!("extended with:{:#?}", exposed_imports);
    make_completion_items(subs, module_id, interns, docs, completions)
}
pub(super) fn get_module_completion_items(
    prefix: String,
    interns: &Interns,
    imported_modules: &HashMap<ModuleId, Arc<Vec<(Symbol, Variable)>>>,
    modules_info: &ModulesInfo,
    docs: &VecMap<ModuleId, ModuleDocumentation>,
    just_modules: bool,
) -> Vec<CompletionItem> {
    let module_completions = imported_modules
        .iter()
        .flat_map(|(mod_id, exposed_symbols)| {
            let mod_name = mod_id.to_ident_str(interns).to_string();

            //Completion for modules themselves
            if mod_name.starts_with(&prefix) {
                let item = CompletionItem {
                    label: mod_name.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    documentation: Some(formatting::module_documentation(
                        formatting::DescriptionsType::Exposes,
                        mod_id,
                        interns,
                        exposed_symbols,
                        docs.get(mod_id),
                        modules_info,
                    )),
                    ..Default::default()
                };
                vec![item]
            //Complete dot completions for module exports
            } else if prefix.starts_with(&(mod_name + ".")) {
                get_module_exposed_completion(
                    exposed_symbols,
                    modules_info,
                    mod_id,
                    docs.get(mod_id),
                    interns,
                )
            } else {
                vec![]
            }
        });
    if just_modules {
        return module_completions.collect();
    }
    module_completions.collect()
}

fn get_module_exposed_completion(
    exposed_symbols: &[(Symbol, Variable)],
    modules_info: &ModulesInfo,
    mod_id: &ModuleId,
    docs: Option<&ModuleDocumentation>,
    interns: &Interns,
) -> Vec<CompletionItem> {
    let mut completion_docs = docs.map_or(Default::default(), |docs| {
        get_completion_docs(exposed_symbols, docs)
    });
    exposed_symbols
        .iter()
        .map(|(sym, var)| {
            //We need to fetch the subs for the module that is exposing what we are trying to complete because that will have the type info we need
            modules_info
                .with_subs(mod_id, |subs| {
                    make_completion_item(
                        subs,
                        mod_id,
                        interns,
                        completion_docs.remove(&sym),
                        sym.as_str(interns).to_string(),
                        *var,
                    )
                })
                .expect("Couldn't find subs for module during completion.")
        })
        .collect::<Vec<_>>()
}

///Provides a list of completions for Type aliases within the scope.
///TODO: Use this when we know we are within a type definition
fn _alias_completions(
    aliases: &MutMap<Symbol, (bool, Alias)>,
    module_id: &ModuleId,
    interns: &Interns,
) -> Vec<CompletionItem> {
    aliases
        .iter()
        .filter(|(symbol, (_exposed, _alias))| &symbol.module_id() == module_id)
        .map(|(symbol, (_exposed, _alias))| {
            let name = symbol.as_str(interns).to_string();
            CompletionItem {
                label: name.clone(),
                detail: Some(name + "we don't know how to print types "),
                kind: Some(CompletionItemKind::CLASS),
                ..Default::default()
            }
        })
        .collect()
}
fn make_completion_items(
    subs: &mut Subs,
    module_id: &ModuleId,
    interns: &Interns,
    docs: Option<&ModuleDocumentation>,
    completions: Vec<(Symbol, Variable)>,
) -> Vec<CompletionItem> {
    let mut completion_docs = docs.map_or(Default::default(), |docs| {
        get_completion_docs(&completions, docs)
    });

    completions
        .into_iter()
        .map(|(symbol, var)| {
            make_completion_item(
                subs,
                module_id,
                interns,
                completion_docs.remove(&symbol),
                symbol.as_str(interns).to_string(),
                var,
            )
        })
        .collect()
}
fn make_completion_items_string(
    subs: &mut Subs,
    module_id: &ModuleId,
    interns: &Interns,
    completions: Vec<(String, Variable)>,
) -> Vec<CompletionItem> {
    completions
        .into_iter()
        .map(|(symbol, var)| make_completion_item(subs, module_id, interns, None, symbol, var))
        .collect()
}
///This efficently walks the list of docs checking fewer times
fn get_completion_docs(
    completions: &[(Symbol, Variable)],
    docs: &ModuleDocumentation,
) -> HashMap<Symbol, String> {
    let mut symbols = completions.iter().map(|(s, _)| s).collect::<Vec<_>>();
    docs.entries
        .iter()
        .filter_map(|doc| match doc {
            roc_load::docs::DocEntry::DocDef(DocDef { docs, symbol, .. }) => {
                let docs = docs.clone()?;
                let (idx, s) = symbols.iter().enumerate().find(|(i, s)| s == &&symbol)?;
                symbols.swap_remove(idx);
                Some((*symbol, docs))
            }
            _ => None,
        })
        .collect()
}

///Finds the types of and names of all the fields of a record
///`var` should be a `Variable` that you know is a record's type or else it will return an empty list
fn find_record_fields(var: Variable, subs: &mut Subs) -> Vec<(String, Variable)> {
    let content = subs.get(var);
    match content.content {
        roc_types::subs::Content::Structure(typ) => match typ {
            roc_types::subs::FlatType::Record(fields, ext) => {
                let field_types = fields.unsorted_iterator(subs, ext);

                match field_types {
                    Ok(field) => field
                        .map(|a| (a.0.clone().into(), a.1.into_inner()))
                        .collect::<Vec<_>>(),
                    Err(err) => {
                        warn!("Error getting record field types for completion: {:?}", err);
                        vec![]
                    }
                }
            }
            roc_types::subs::FlatType::Tuple(elems, ext) => {
                let elems = elems.unsorted_iterator(subs, ext);

                match elems {
                    Ok(elem) => elem.map(|(num, var)| (num.to_string(), var)).collect(),
                    Err(err) => {
                        warn!("Error getting tuple elems for completion: {:?}", err);
                        vec![]
                    }
                }
            }

            _ => {
                warn!(
                    "Trying to get field completion for a type that is not a record: {:?}",
                    typ
                );
                vec![]
            }
        },
        roc_types::subs::Content::Error => {
            //This is caused by typechecking our partially typed variable name causing the typechecking to be confused as the type of the parent variable
            //TODO! ideally i could recover using some previous typecheck result that isn't broken
            warn!("Variable type of record was of type 'error', cannot access field",);
            vec![]
        }
        _ => {
            warn!(
                "Variable before field was unsupported type: {:?}",
                subs.dbg(var)
            );
            vec![]
        }
    }
}

struct FieldCompletion {
    ///The name of the variable that is a record
    variable_name: String,
    field: String,
    middle_fields: Vec<String>,
}
///Splits a completion prefix for a field into its components
///E.g. a.b.c.d->{variable_name:"a",middle_fields:["b","c"],field:"d"}
fn get_field_completion_parts(symbol_prefix: &str) -> Option<FieldCompletion> {
    let mut parts = symbol_prefix.split('.').collect::<Vec<_>>();
    let field = parts.pop().unwrap_or("").to_string();
    let variable_name = parts.remove(0).to_string();
    //Now that we have the head and tail removed  this is all the intermediate fields
    let middle_fields = parts.into_iter().map(ToString::to_string).collect();

    Some(FieldCompletion {
        variable_name,
        field,
        middle_fields,
    })
}
pub fn field_completion(
    position: Position,
    symbol_prefix: String,
    declarations: &Declarations,
    interns: &Interns,
    subs: &mut Subs,
    module_id: &ModuleId,
) -> Option<Vec<CompletionItem>> {
    let FieldCompletion {
        variable_name,
        field,
        middle_fields,
    } = get_field_completion_parts(&symbol_prefix)?;

    debug!(
        "Getting record field completions: variable: {:?} field: {:?} middle: {:?} ",
        variable_name, field, middle_fields
    );

    //We get completetions here, but all we really want is the info about the variable that is the first part of our record completion.
    //We are completing the full name of the variable so we should only have one match
    let completion = get_completions(position, declarations, variable_name, interns)
        .into_iter()
        .map(|(symb, var)| (symb.as_str(interns).to_string(), var))
        .next()?;

    //If we have a type that has nested records we could have a completion prefix like: "var.field1.field2.fi"
    //If the document isn't fully typechecked we won't know what the type of field2 is for us to offer completions based on it's fields
    //Instead we get the type of "var" and then the type of "field1" within var's type and then "field2" within field1's type etc etc, until we have the type of the record we are actually looking for field completions for.
    let completion_record = middle_fields.iter().fold(completion, |state, chain_field| {
        let fields_vars = find_record_fields(state.1, subs);
        fields_vars
            .into_iter()
            .find(|type_field| chain_field == &type_field.0)
            .unwrap_or(state)
    });

    let field_completions: Vec<_> = find_record_fields(completion_record.1, subs)
        .into_iter()
        .filter(|(str, _)| str.starts_with(&field.to_string()))
        .collect();

    let field_completions =
        make_completion_items_string(subs, module_id, interns, field_completions);
    Some(field_completions)
}
