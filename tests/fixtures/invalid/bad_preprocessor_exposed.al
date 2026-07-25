// Invalid: Preprocessor does NOT hide this invalid code (#if 1 = active)
codeunit 50205 "Bad Preprocessor"
{
    #if 1
    this is not valid AL { !!! !!!
    #endif

    procedure Foo()
    begin
        exit();
    end;
}
