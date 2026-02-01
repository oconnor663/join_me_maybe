use join_me_maybe::join;
use std::future::ready;

#[tokio::main]
async fn main() {
    join!(
        x = ready(Some(true)) => {
            // TODO: Unfortunately an explicit type annotation is needed here. See also
            // `test_type_annotation_needed`.
            x.unwrap();
        }
    );
}
