//! First survival loop. Rates are per simulated gameplay second, independent of FPS.
use crate::terrain::Block;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Item { Dirt, Stone, Wood, Fiber, Berries, Planks, Campfire, StonePick,
    StoneAxe, CookedBerries, CopperOre, CopperIngot, CopperPick }
pub const ITEM_COUNT:usize=13;
pub const ITEMS:[Item;ITEM_COUNT]=[Item::Dirt,Item::Stone,Item::Wood,Item::Fiber,
    Item::Berries,Item::Planks,Item::Campfire,Item::StonePick,Item::StoneAxe,
    Item::CookedBerries,Item::CopperOre,Item::CopperIngot,Item::CopperPick];
impl Item {
    pub fn name(self)-> &'static str {match self {
        Self::Dirt=>"Dirt",Self::Stone=>"Stone",Self::Wood=>"Wood",Self::Fiber=>"Fiber",
        Self::Berries=>"Berries",Self::Planks=>"Planks",Self::Campfire=>"Campfire",
        Self::StonePick=>"Stone pick",Self::StoneAxe=>"Stone axe",
        Self::CookedBerries=>"Cooked berries",Self::CopperOre=>"Copper ore",
        Self::CopperIngot=>"Copper ingot",Self::CopperPick=>"Copper pick",
    }}
    pub fn block(self)->Option<Block> {match self {
        Self::Dirt=>Some(Block::Dirt),Self::Stone=>Some(Block::Stone),
        Self::Wood=>Some(Block::Wood),Self::Planks=>Some(Block::Planks),
        Self::Campfire=>Some(Block::Campfire),_=>None,
    }}
}
pub struct Recipe {pub name:&'static str,pub cost:&'static [(Item,u16)],pub output:(Item,u16),pub fire:bool}
pub const RECIPES:[Recipe;8]=[
    Recipe{name:"4 planks",cost:&[(Item::Wood,1)],output:(Item::Planks,4),fire:false},
    Recipe{name:"Stone pick",cost:&[(Item::Stone,3),(Item::Wood,2),(Item::Fiber,2)],output:(Item::StonePick,1),fire:false},
    Recipe{name:"Stone axe",cost:&[(Item::Stone,2),(Item::Wood,2),(Item::Fiber,2)],output:(Item::StoneAxe,1),fire:false},
    Recipe{name:"Campfire",cost:&[(Item::Stone,4),(Item::Wood,3)],output:(Item::Campfire,1),fire:false},
    Recipe{name:"Cook 3 berries",cost:&[(Item::Berries,3),(Item::Wood,1)],output:(Item::CookedBerries,3),fire:true},
    Recipe{name:"Smelt copper",cost:&[(Item::CopperOre,3),(Item::Wood,3)],output:(Item::CopperIngot,1),fire:true},
    Recipe{name:"Copper pick",cost:&[(Item::CopperIngot,3),(Item::Wood,2)],output:(Item::CopperPick,1),fire:true},
    Recipe{name:"4 fiber from wood",cost:&[(Item::Wood,1)],output:(Item::Fiber,4),fire:false},
];
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Survival {
    pub counts:[u16;ITEM_COUNT],
    pub health:f32,pub hunger:f32,pub thirst:f32,pub stamina:f32,
    pub stone_pick_uses:u16,pub stone_axe_uses:u16,pub copper_pick_uses:u16,
    pub selected:usize,pub recipe:usize,
}
impl Default for Survival {
    fn default()->Self {Self{counts:[0;ITEM_COUNT],health:100.0,hunger:100.0,
        thirst:100.0,stamina:100.0,stone_pick_uses:0,stone_axe_uses:0,
        copper_pick_uses:0,selected:0,recipe:0}}
}
impl Survival {
    pub fn valid(&self)->bool {
        [self.health,self.hunger,self.thirst,self.stamina].iter().all(|v|v.is_finite()&&(0.0..=100.0).contains(v))
        && self.counts.iter().all(|&v|v<=999) && self.selected<ITEM_COUNT && self.recipe<RECIPES.len()
        && self.stone_pick_uses<=80 && self.stone_axe_uses<=80 && self.copper_pick_uses<=240
    }
    pub fn dead(&self)->bool {self.health<=0.0}
    pub fn selected_item(&self)->Item {ITEMS[self.selected]}
    pub fn count(&self,i:Item)->u16 {self.counts[i as usize]}
    pub fn add(&mut self,i:Item,n:u16)->bool {
        if self.count(i) as u32+n as u32>999 {return false;}
        self.counts[i as usize]+=n;true
    }
    pub fn consume(&mut self,i:Item,n:u16)->bool {
        if self.count(i)<n {return false;}
        self.counts[i as usize]-=n;true
    }
    pub fn tick(&mut self,dt:f32,moving:bool) {
        if self.dead() || !dt.is_finite() || dt<=0.0 {return;}
        self.hunger=(self.hunger-dt/36.0).max(0.0);
        self.thirst=(self.thirst-dt/24.0).max(0.0);
        self.stamina=(self.stamina+dt*if moving {2.0}else{8.0}).min(100.0);
        if self.hunger==0.0 || self.thirst==0.0 {self.health=(self.health-dt*0.5).max(0.0);}
        else if self.hunger>60.0 && self.thirst>60.0 {self.health=(self.health+dt*0.1).min(100.0);}
    }
    pub fn craft(&mut self,fire:bool)->Result<(), &'static str> {
        let r=&RECIPES[self.recipe];
        if self.dead(){return Err("Respawn first");}
        if r.fire&&!fire{return Err("Place a campfire and stand within 3m");}
        if r.cost.iter().any(|&(i,n)|self.count(i)<n){return Err("Missing ingredients (see recipe)");}
        if self.count(r.output.0) as u32+r.output.1 as u32>999{return Err("Output stack full");}
        for &(i,n) in r.cost {self.consume(i,n);}
        self.add(r.output.0,r.output.1);Ok(())
    }
    pub fn eat(&mut self)->bool {
        if self.dead() || self.hunger>=100.0{return false;}
        let food=if self.count(Item::CookedBerries)>0 {Item::CookedBerries}else{Item::Berries};
        if !self.consume(food,1){return false;}
        self.hunger=(self.hunger+if food==Item::Berries {8.0}else{20.0}).min(100.0);
        true
    }
    /// Check the entire mining transaction before removing a world block.
    pub fn harvest(&mut self,b:Block)->Result<(), &'static str> {
        if self.dead(){return Err("Respawn first");}
        if self.stamina<4.0{return Err("Rest to recover stamina");}
        let drops:&[(Item,u16)]=match b {
            Block::Dirt|Block::Grass=>&[(Item::Dirt,1)],Block::Stone=>&[(Item::Stone,1)],
            Block::Wood=>&[(Item::Wood,1)],Block::Leaves=>&[(Item::Fiber,1),(Item::Berries,1)],
            Block::Planks=>&[(Item::Planks,1)],Block::Campfire=>&[(Item::Campfire,1)],
            Block::CopperOre=>&[(Item::CopperOre,1)],_=>return Err("Nothing to gather"),
        };
        if drops.iter().any(|&(i,n)|self.count(i) as u32+n as u32>999){return Err("Inventory stack full");}
        let pick=b==Block::Stone||b==Block::CopperOre;
        if b==Block::CopperOre && self.stone_pick_uses==0 && self.copper_pick_uses==0
            && self.count(Item::StonePick)==0 && self.count(Item::CopperPick)==0 {return Err("Copper ore needs a pick");}
        if pick {
            if self.copper_pick_uses==0 && self.consume(Item::CopperPick,1){self.copper_pick_uses=240;}
            if self.copper_pick_uses>0{self.copper_pick_uses-=1;}
            else {
                if self.stone_pick_uses==0 && self.consume(Item::StonePick,1){self.stone_pick_uses=80;}
                if self.stone_pick_uses>0{self.stone_pick_uses-=1;}
            }
        }
        if b==Block::Wood {
            if self.stone_axe_uses==0 && self.consume(Item::StoneAxe,1){self.stone_axe_uses=80;}
            if self.stone_axe_uses>0 {self.stone_axe_uses-=1;}
        }
        for &(i,n) in drops {self.add(i,n);}
        self.stamina-=4.0;Ok(())
    }
    pub fn mining_seconds(&self,b:Block)->f32 {match b {
        Block::Stone|Block::CopperOre=>if self.copper_pick_uses>0||self.count(Item::CopperPick)>0 {0.25}
            else if self.stone_pick_uses>0||self.count(Item::StonePick)>0 {0.5}else{1.5},
        Block::Wood=>if self.stone_axe_uses>0||self.count(Item::StoneAxe)>0 {0.3}else{1.0},
        _=>0.25,
    }}
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn failed_craft_and_harvest_are_atomic() {
        let mut s=Survival::default();s.recipe=1;let before=s.clone();
        assert!(s.craft(false).is_err());assert_eq!(s,before);
        assert!(s.harvest(Block::CopperOre).is_err());assert_eq!(s,before);
        s.counts[Item::Berries as usize]=999;let before=s.clone();
        assert!(s.harvest(Block::Leaves).is_err());assert_eq!(s,before);
    }
    #[test] fn primitive_to_copper_progression_consumes_resources() {
        let mut s=Survival::default();
        for i in [Item::Wood,Item::Stone,Item::Fiber] {s.add(i,50);}
        s.recipe=1;s.craft(false).unwrap();
        for _ in 0..9 {s.harvest(Block::CopperOre).unwrap();}
        s.recipe=5;assert!(s.craft(false).is_err());
        for _ in 0..3 {s.craft(true).unwrap();}
        s.recipe=6;s.craft(true).unwrap();
        assert_eq!(s.count(Item::CopperPick),1);assert_eq!(s.count(Item::CopperOre),0);
        assert_eq!(s.count(Item::CopperIngot),0);assert_eq!(s.stone_pick_uses,71);
    }
    #[test] fn survival_is_frame_independent_and_death_stops_actions() {
        let mut a=Survival::default();let mut b=a.clone();
        for _ in 0..600{a.tick(0.1,false);}for _ in 0..60{b.tick(1.0,false);}
        assert!((a.thirst-b.thirst).abs()<0.01);
        a.health=0.0;a.add(Item::Berries,1);a.hunger=0.0;
        assert!(!a.eat());assert!(a.harvest(Block::Leaves).is_err());
    }
}
