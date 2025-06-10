use game_mode::config::*;

fn main() {
    println!("GREETD_DIR='{}'", GREETD_DIR);
    println!("CONFIG_FILE='{}'", CONFIG_FILE);
    println!("GAME_MODE_CONFIG='{}'", GAME_MODE_CONFIG);
    println!("GREETER_USER='{}'", GREETER_USER);
    println!("VT_NUMBER={}", VT_NUMBER);
    println!("DEBUG_MODE={}", DEBUG_MODE);
    println!("REQUIRED_GROUPS=({})", REQUIRED_GROUPS.join(" "));
    println!("GAMES_USER='{}'", GAMES_USER);
    println!("GAMES_GROUP='{}'", GAMES_GROUP);
    println!("GAMES_DIR='{}'", GAMES_DIR);
} 