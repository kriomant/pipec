fn main() {
    let out = format!("{}/pipeline_ebnf.rs", std::env::var("OUT_DIR").unwrap());

    peginator_codegen::Compile::file("pipeline.ebnf")
        .destination(out)
        .format()
        .run_exit_on_error();

    println!("cargo:rerun-if-changed=pipeline.ebnf");
}
