//! Names
//! =====
//! An agent's name: a common adjective and noun, capitalized, joined
//! (FireAnt; older ones Fire-Ant): 16,384; one taken is drawn again.

use rand::seq::SliceRandom;

const ADJS: &str = "
red blue green yellow orange purple pink brown black white gray silver golden
amber azure bronze copper jade lemon lime mint navy peach plum ruby violet
cherry honey big small tiny huge giant little tall short long round flat thick
thin deep soft hard smooth rough fuzzy fluffy shiny crunchy crispy leafy spiky
hot cold warm cool icy frosty snowy rainy sunny cloudy windy stormy foggy
misty sweet sour salty spicy juicy minty nutty creamy smoky tasty fresh bright
dark quiet loud happy jolly merry calm brave bold clever wise kind gentle
lucky proud quick swift fast slow eager busy sleepy silly funny shy neat noble
grand royal mighty strong steady nimble lively cozy old new young ancient
wooden stone paper iron magic cosmic lunar solar polar wild";

const NOUNS: &str = "
ant bat bear bee beetle bird bison camel cat cheetah chicken clam cobra cougar
cow crab crane crow deer dog dolphin donkey dove dragon duck eagle eel elk
falcon finch fish flamingo fox frog gazelle gecko giraffe goat goose gorilla
hamster hare hawk hedgehog heron hippo horse hyena iguana jaguar jay kangaroo
kitten koala lamb lemur leopard lion lizard llama lobster moose moth mouse
mule octopus orca otter owl ox oyster panda parrot pelican penguin pig pigeon
pony puffin puma rabbit raccoon raven rhino robin salmon seal shark sheep
shrimp skunk sloth snail snake sparrow spider squid squirrel swan tiger toad
toucan trout tuna turkey turtle walrus wasp whale wolf wombat yak zebra apple
banana bagel cookie mango melon muffin noodle pancake pepper potato pretzel
radish taco waffle";

pub fn random() -> String {
    let adjs: Vec<&str> = ADJS.split_whitespace().collect();
    let nouns: Vec<&str> = NOUNS.split_whitespace().collect();
    let mut rng = rand::thread_rng();
    word(adjs.choose(&mut rng).unwrap()) + &word(nouns.choose(&mut rng).unwrap())
}

/// Whether a text is a name, old or new.
pub fn is_name(s: &str) -> bool {
    let words = |list: &str| list.split_whitespace().map(word).collect::<Vec<_>>();
    let (adjs, nouns) = (words(ADJS), words(NOUNS));
    adjs.iter().any(|a| {
        s.strip_prefix(a.as_str())
            .map(|r| r.strip_prefix('-').unwrap_or(r))
            .is_some_and(|r| nouns.iter().any(|n| n == r))
    })
}

fn word(w: &str) -> String {
    let mut c = w.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_read_back() {
        let n = super::random();
        assert!(super::is_name(&n));
        assert!(!super::is_name("Fire-Ant") && super::is_name("Red-Ant"));
    }
}
