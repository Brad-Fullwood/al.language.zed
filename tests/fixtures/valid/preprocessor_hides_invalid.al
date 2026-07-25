codeunit 50111 Valid_Preprocessor
{
    #if 0
    this is not valid AL { !!! !!!
    #endif

    procedure Foo();
    begin
        exit();
    end;
}

