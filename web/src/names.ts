// Names
// =====
// An agent's name: a common adjective and noun, capitalized, joined
// (FireAnt; older ones Fire-Ant). The same lists as core/src/names.rs.

const ADJS = `
red blue green yellow orange purple pink brown black white gray silver
golden amber azure bronze copper jade lemon lime mint navy peach plum ruby
violet cherry honey big small tiny huge giant little tall short long round
flat thick thin deep soft hard smooth rough fuzzy fluffy shiny crunchy
crispy leafy spiky hot cold warm cool icy frosty snowy rainy sunny cloudy
windy stormy foggy misty sweet sour salty spicy juicy minty nutty creamy
smoky tasty fresh bright dark quiet loud happy jolly merry calm brave bold
clever wise kind gentle lucky proud quick swift fast slow eager busy sleepy
silly funny shy neat noble grand royal mighty strong steady nimble lively
cozy old new young ancient wooden stone paper iron magic cosmic lunar solar
polar wild`;

const NOUNS = `
ant bat bear bee beetle bird bison camel cat cheetah chicken clam cobra
cougar cow crab crane crow deer dog dolphin donkey dove dragon duck eagle
eel elk falcon finch fish flamingo fox frog gazelle gecko giraffe goat goose
gorilla hamster hare hawk hedgehog heron hippo horse hyena iguana jaguar jay
kangaroo kitten koala lamb lemur leopard lion lizard llama lobster moose
moth mouse mule octopus orca otter owl ox oyster panda parrot pelican
penguin pig pigeon pony puffin puma rabbit raccoon raven rhino robin salmon
seal shark sheep shrimp skunk sloth snail snake sparrow spider squid
squirrel swan tiger toad toucan trout tuna turkey turtle walrus wasp whale
wolf wombat yak zebra apple banana bagel cookie mango melon muffin noodle
pancake pepper potato pretzel radish taco waffle`;

const word = (w: string) => w.charAt(0).toUpperCase() + w.slice(1);
const adjs = ADJS.split(/\s+/).filter(Boolean).map(word);
const nouns = new Set(NOUNS.split(/\s+/).filter(Boolean).map(word));

/** Whether a text is a name, old or new. */
export function isName(s: string): boolean {
  return adjs.some((a) => {
    if (!s.startsWith(a)) return false;
    const r = s.slice(a.length);
    return nouns.has(r.startsWith('-') ? r.slice(1) : r);
  });
}
