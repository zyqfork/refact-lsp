#include <stdio.h>
#include <memory>

class Animal {
public:
    int age;

    Animal(int age_)
    {
        age = age_;
    }
};

class HasMass {
public:
    float mass;

    HasMass(float mass):
        mass(mass)
    {
    }
};

class CompiledFrog: public Animal, public HasMass {
public:
    CompiledFrog(int age, float mass):
        Animal(age),
        HasMass(mass)
    {
    }

    void say_hi() const
    {
        printf("I am a frog! age=%d mass=%0.2f\n", age, mass);
    }
};

void some_fun(CompiledFrog* f1, CompiledFrog& f2, const CompiledFrog& f3, const std::shared_ptr<CompiledFrog>& f4)
{
    CompiledFrog local_frog(7, 666.0);
    f1->say_hi();
    f2.say_hi();
    f3.say_hi();
    f4->say_hi();
    local_frog.say_hi();
}

int main()
{
    CompiledFrog teh_frog(5, 13.37);
    std::shared_ptr<CompiledFrog> shared_frog = std::make_shared<CompiledFrog>(6, 42.0);
    some_fun(&teh_frog, teh_frog, teh_frog, shared_frog);
}
